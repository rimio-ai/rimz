use super::*;

#[cfg(unix)]
use super::backend::reconcile_pane;
pub(crate) mod support;

#[cfg(unix)]
use self::support::{command_count, shim_log, zellij_shim};

#[cfg(unix)]
use crate::config::MultiplexerConfig;
#[cfg(unix)]
use crate::ids::{PaneId, WorkspaceId};
#[cfg(unix)]
use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane, TopologyClients};
#[cfg(unix)]
use crate::mux::{
    LayoutColumn, LayoutPanes, MuxBackend, PaneCmd, PaneListOptions, PaneReadConsistency,
    ReconcilePaneRole, SidebarLiveness, SidebarPaneOptions, SidebarWidth, SplitDirection,
    SplitPaneOptions, SplitPlacement, SplitTarget, TabOptions, WidthSyncOptions,
};
#[cfg(unix)]
use crate::sidebar::cache::write_pane_topology_cache;
#[cfg(unix)]
use crate::sidebar::timing::unix_now_ms;
#[cfg(unix)]
use crate::store::paths::RuntimePaths;

#[cfg(unix)]
struct TestRoom {
    runtime_root: tempfile::TempDir,
    project_root: tempfile::TempDir,
    workspace_id: WorkspaceId,
    runtime: RuntimePaths,
}

#[cfg(unix)]
impl TestRoom {
    fn new() -> Self {
        let runtime_root = tempfile::TempDir::new().expect("runtime tempdir");
        let project_root = tempfile::TempDir::new().expect("project tempdir");
        let workspace_id = WorkspaceId::from_project_root(project_root.path());
        let runtime =
            RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
        runtime.ensure_dirs().expect("runtime dirs");
        Self {
            runtime_root,
            project_root,
            workspace_id,
            runtime,
        }
    }

    fn backend(&self, shim: &std::path::Path) -> ZellijBackend {
        ZellijBackend::with_program_and_runtime_for_test(shim, self.runtime_root.path())
            .with_presence_plugin_for_test(shim)
    }

    fn write_cache(
        &self,
        produced_at_ms: u64,
        focused_pane: Option<u64>,
        clients: Option<TopologyClients>,
        panes: Vec<PaneTopologyPane>,
    ) {
        write_pane_topology_cache(
            &self.runtime,
            &PaneTopologyCache {
                session_name: "rimz-test".to_owned(),
                produced_at_ms,
                writer: None,
                focused_pane,
                clients,
                panes,
            },
        )
        .expect("write topology cache");
    }

    fn sidebar_options(&self, view_cols: u16) -> SidebarPaneOptions {
        let width = SidebarWidth::default();
        let view_cols = std::num::NonZeroU16::new(view_cols).expect("nonzero test view");
        let requested_cols = std::num::NonZeroU16::new(
            u16::try_from(width.target_cols(u64::from(view_cols.get()))).expect("test target"),
        )
        .expect("nonzero test width");
        let share = crate::mux::WidthPermille::from_cols(requested_cols, view_cols);
        SidebarPaneOptions {
            session_name: "rimz-test".to_owned(),
            workspace_id: self.workspace_id.clone(),
            project_root: self.project_root.path().to_path_buf(),
            extra_env: Default::default(),
            cwd: self.project_root.path().to_path_buf(),
            target: crate::mux::SidebarTarget {
                share,
                max_cols: width.max_cols,
                pinned: false,
            },
            detected_view_size: None,
            rimz_bin: "rimz".into(),
            pristine_birth: false,
            config: MultiplexerConfig::default(),
            resume_tabs: Vec::new(),
            refresh_ms: None,
        }
    }
}

#[test]
fn existing_session_attach_has_no_creation_or_options_tail() {
    let spec = ZellijBackend::new().attach_existing_command("rimz-test");

    assert_eq!(spec.args, ["attach", "rimz-test"]);
    assert!(!spec.args.iter().any(|arg| arg == "--create"));
    assert!(!spec.args.iter().any(|arg| arg == "options"));
}

#[test]
fn readonly_attach_relies_on_the_broadcast_ttyd_input_boundary() {
    let spec = ZellijBackend::new().attach_readonly_command("rimz-test");

    assert_eq!(spec.args, ["attach", "rimz-test"]);
}

#[cfg(unix)]
#[test]
fn rename_tab_resolves_the_anchor_to_a_stable_id() {
    let (temp, shim) =
        support::pane_roster_shim(r#"[{"id":7,"is_plugin":false,"tab_id":42,"tab_position":3}]"#);
    let backend = ZellijBackend::with_program_for_test(&shim);
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_7");

    backend
        .rename_tab("rimz-test", &pane, "#feat ✓")
        .expect("rename by stable tab id");

    let log = shim_log(&temp);
    assert!(
        log.contains("--session rimz-test action rename-tab-by-id 42 #feat ✓"),
        "{log}"
    );
}

#[cfg(unix)]
fn terminal_pane(
    id: u64,
    tab_position: u64,
    pane_columns: u64,
    pane_x: u64,
    title: &str,
) -> PaneTopologyPane {
    PaneTopologyPane {
        id,
        is_plugin: false,
        is_held: false,
        exited: false,
        is_suppressed: false,
        is_floating: false,
        tab_position,
        tab_name: Some("work".to_owned()),
        pane_columns: Some(pane_columns),
        pane_x: Some(pane_x),
        title: Some(title.to_owned()),
        pane_command: None,
        pane_cwd: None,
        pane_pid: None,
        terminal_command: None,
    }
}

#[cfg(unix)]
#[test]
fn reconcile_mapping_classifies_native_sidebar_daemon_and_work_panes() {
    let mut sidebar = terminal_pane(1, 4, 20, 0, crate::pane::SIDEBAR_CHROME_TITLE);
    sidebar.is_held = true;
    let mut named_daemon = terminal_pane(2, 5, 80, 20, "host");
    named_daemon.tab_name = Some(crate::daemon_view::VIEW_NAME.to_owned());
    let mut command_daemon = terminal_pane(3, 6, 80, 20, "host");
    command_daemon.terminal_command = Some("rimz app-server serve".to_owned());
    let mut work = terminal_pane(4, 7, 80, 20, "work");
    work.exited = true;
    let mut plugin = terminal_pane(5, 8, 80, 20, "plugin");
    plugin.is_plugin = true;

    let mapped = [
        reconcile_pane(&sidebar).expect("held sidebar remains structural"),
        reconcile_pane(&named_daemon).expect("named daemon remains structural"),
        reconcile_pane(&command_daemon).expect("command daemon remains structural"),
        reconcile_pane(&work).expect("exited work pane remains structural"),
    ];
    assert_eq!(mapped[0].role, ReconcilePaneRole::Sidebar);
    assert_eq!(mapped[1].role, ReconcilePaneRole::DaemonHost);
    assert_eq!(mapped[2].role, ReconcilePaneRole::DaemonHost);
    assert_eq!(mapped[3].role, ReconcilePaneRole::Working);
    assert!(reconcile_pane(&plugin).is_none());
}

fn option_map(args: &[String]) -> std::collections::BTreeMap<&str, &str> {
    assert!(
        args.len().is_multiple_of(2),
        "option argv must be pairs: {args:?}"
    );
    args.chunks_exact(2)
        .map(|pair| (pair[0].as_str(), pair[1].as_str()))
        .collect()
}

fn expected_option_map(spec: &str) -> std::collections::BTreeMap<&str, &str> {
    spec.split_whitespace()
        .map(|entry| entry.split_once('=').expect("flag=value"))
        .collect()
}

#[cfg(unix)]
#[test]
fn split_pane_routes_directional_and_anchored_requests() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s | pane=%s\n' "$*" "$ZELLIJ_PANE_ID" >> "$dir/zellij.log"
case " $* " in
  *" action list-panes --all --json "*)
    printf '[{"id":7,"is_plugin":false,"tab_id":42,"tab_position":3}]\n' ;;
esac
exit 0
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);
    for direction in [SplitDirection::Right, SplitDirection::Down] {
        backend
            .split_pane(SplitPaneOptions {
                placement: SplitPlacement::Directional(direction),
                focus: true,
                title: Some("rimz managed pane".to_owned()),
                ..Default::default()
            })
            .expect("directional split");
    }
    backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "rimz-test".to_owned(),
                pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
            },
            placement: SplitPlacement::Directional(SplitDirection::Right),
            focus: true,
            ..Default::default()
        })
        .expect("explicit session-pane split");
    backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: "rimz-test".to_owned(),
                pane_id: PaneId::from_parts(crate::MuxName::Zellij, "terminal_7"),
            },
            placement: SplitPlacement::Stacked,
            focus: false,
            ..Default::default()
        })
        .expect("anchored stack");

    let log = shim_log(&temp);
    for command in [
        "action new-pane --direction right --name rimz managed pane",
        "action new-pane --direction down --name rimz managed pane",
    ] {
        assert!(log.contains(command), "{log}");
    }
    assert!(
        log.contains("--session rimz-test action new-pane --direction right --tab-id 42 | pane=7"),
        "{log}"
    );
    let anchored = log.lines().last().expect("anchored command");
    assert!(
        anchored.contains("action new-pane --stacked --near-current-pane | pane=7"),
        "{log}"
    );
    assert!(
        !anchored.contains("--tab-id") && !log.contains("focus-pane-id"),
        "{log}"
    );
    assert_eq!(
        log.lines().count(),
        5,
        "expected split and lookup calls:\n{log}"
    );
}

#[cfg(unix)]
#[test]
fn split_pane_emits_close_on_exit_only_when_requested() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
exit 0
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);

    backend
        .split_pane(SplitPaneOptions {
            close_on_exit: true,
            focus: true,
            ..Default::default()
        })
        .expect("self-closing split");
    backend
        .split_pane(SplitPaneOptions {
            focus: true,
            ..Default::default()
        })
        .expect("default split");

    let log = shim_log(&temp);
    let mut commands = log.lines();
    assert!(
        commands
            .next()
            .is_some_and(|command| command.contains("--close-on-exit")),
        "{log}"
    );
    assert!(
        commands
            .next()
            .is_some_and(|command| !command.contains("--close-on-exit")),
        "{log}"
    );
    assert!(commands.next().is_none(), "{log}");
}

#[cfg(unix)]
#[test]
fn list_panes_uses_fresh_topology_and_honors_explicit_floor() {
    let room = TestRoom::new();
    let floor = unix_now_ms();
    room.write_cache(
        floor.saturating_sub(1),
        Some(7),
        Some(TopologyClients {
            human_clients: Some(2),
            viewed_panes: Some(vec![7]),
            views: Vec::new(),
        }),
        vec![PaneTopologyPane {
            pane_command: Some("zsh".to_owned()),
            pane_cwd: Some(room.project_root.path().to_string_lossy().into_owned()),
            terminal_command: None,
            ..terminal_pane(7, 0, 100, 0, "zsh")
        }],
    );
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
exit 1
"#,
    );
    let backend = room.backend(&shim);
    let listing = backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-test".to_owned()),
            workspace_id: Some(room.workspace_id.clone()),
            ..Default::default()
        })
        .expect("fresh topology");
    assert_eq!(listing.panes.len(), 1);
    let pane = &listing.panes[0];
    assert_eq!(
        (pane.pane_id.raw(), pane.view_id.as_deref()),
        ("terminal_7", Some("tab_0"))
    );
    assert_eq!(
        (pane.command.as_deref(), pane.spawn_command.as_deref()),
        (Some("zsh"), None)
    );
    assert_eq!(
        pane.cwd.as_deref(),
        Some(room.project_root.path().to_string_lossy().as_ref())
    );
    let client = listing.client_view.expect("client view");
    assert_eq!(
        (client.presence.human_clients, client.presence.last_input_ms),
        (2, None)
    );
    assert_eq!(client.viewed_panes, vec![pane.pane_id.clone()]);
    assert!(
        shim_log(&temp).is_empty(),
        "fresh cache must avoid Zellij actions"
    );

    backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-test".to_owned()),
            workspace_id: Some(room.workspace_id.clone()),
            min_topology_produced_at_ms: Some(floor),
            command_timeout: Some(Duration::from_millis(1)),
            ..Default::default()
        })
        .expect_err("explicit floor rejects pre-floor cache");
}

#[cfg(unix)]
#[test]
fn reconcile_sidebars_rejects_topology_before_the_liveness_floor() {
    let room = TestRoom::new();
    let produced_at_ms = unix_now_ms();
    room.write_cache(
        produced_at_ms,
        Some(1),
        None,
        vec![
            terminal_pane(1, 0, 50, 0, "rimz-sidebar"),
            terminal_pane(2, 0, 150, 50, "work"),
        ],
    );
    let (_temp, shim) = zellij_shim("#!/bin/sh\nexit 1\n");
    let backend = room.backend(&shim);
    let live = SidebarLiveness {
        claimed_panes: [PaneId::from_parts(
            crate::ids::MuxName::Zellij,
            "terminal_1",
        )]
        .into(),
        topology_floor_ms: Some(produced_at_ms.saturating_add(1)),
        ..SidebarLiveness::default()
    };

    backend
        .reconcile_sidebars(&room.sidebar_options(200), &live)
        .expect_err("a repair pass cannot judge geometry from pre-floor topology");
}

#[cfg(unix)]
#[test]
fn floorless_reconcile_skips_width_only_repairs() {
    let room = TestRoom::new();
    room.write_cache(
        unix_now_ms(),
        Some(1),
        None,
        vec![
            terminal_pane(1, 0, 150, 0, "rimz-sidebar"),
            terminal_pane(2, 0, 50, 150, "work"),
        ],
    );
    let (temp, shim) = zellij_shim("#!/bin/sh\nexit 1\n");
    let backend = room.backend(&shim);
    let live = SidebarLiveness {
        claimed_panes: [PaneId::from_parts(
            crate::ids::MuxName::Zellij,
            "terminal_1",
        )]
        .into(),
        ..SidebarLiveness::default()
    };

    let report = backend
        .reconcile_sidebars(&room.sidebar_options(200), &live)
        .expect("floorless structural reconcile");

    assert_eq!(report, crate::mux::SidebarRecovery::default());
    assert!(
        shim_log(&temp).is_empty(),
        "an unproven viewport must not trigger a width action",
    );
}

#[cfg(unix)]
#[test]
fn cached_pane_roster_reads_only_fresh_normalized_terminal_ids() {
    let room = TestRoom::new();
    let produced_at_ms = unix_now_ms();
    room.write_cache(
        produced_at_ms,
        None,
        None,
        vec![
            terminal_pane(7, 0, 80, 0, "zsh"),
            PaneTopologyPane {
                is_plugin: true,
                ..terminal_pane(9, 0, 0, 0, "plugin")
            },
        ],
    );
    let backend = ZellijBackend::with_runtime_dir(room.runtime_root.path());

    assert_eq!(
        backend.cached_pane_roster("rimz-test", &room.workspace_id),
        Some(crate::mux::CachedPaneRoster {
            pane_ids: vec![PaneId::from_parts(
                crate::ids::MuxName::Zellij,
                "terminal_7",
            )],
            observed_at_ms: produced_at_ms,
        }),
    );

    room.write_cache(
        produced_at_ms
            .saturating_sub(crate::sidebar::timing::PRESENCE_STAMP_FRESH.as_millis() as u64)
            .saturating_sub(1),
        None,
        None,
        vec![terminal_pane(7, 0, 80, 0, "zsh")],
    );
    assert_eq!(
        backend.cached_pane_roster("rimz-test", &room.workspace_id),
        None,
    );
}

#[cfg(unix)]
#[test]
fn authoritative_list_panes_preserves_server_identity_and_cache_enrichment() {
    let room = TestRoom::new();
    room.write_cache(
        1,
        None,
        None,
        vec![
            PaneTopologyPane {
                tab_name: Some("old".to_owned()),
                pane_command: Some("vim".to_owned()),
                pane_cwd: Some(room.project_root.path().to_string_lossy().into_owned()),
                pane_pid: Some(707),
                ..terminal_pane(7, 0, 999, 999, "zsh")
            },
            PaneTopologyPane {
                is_plugin: true,
                pane_command: Some("plugin-command".to_owned()),
                pane_cwd: Some("/plugin".to_owned()),
                pane_pid: Some(909),
                ..terminal_pane(7, 0, 888, 888, "plugin")
            },
            PaneTopologyPane {
                pane_command: Some("rimz-sidebar".to_owned()),
                terminal_command: Some("rimz".to_owned()),
                ..terminal_pane(8, 1, 40, 0, "rimz-sidebar")
            },
            PaneTopologyPane {
                pane_command: Some("rimz-sidebar".to_owned()),
                terminal_command: Some("rimz".to_owned()),
                ..terminal_pane(9, 2, 50, 0, "rimz-sidebar")
            },
        ],
    );
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
case " $* " in
  *" action list-panes --all --json "*)
    printf '[{"id":7,"is_plugin":true,"tab_position":0,"tab_name":"work","title":"plugin"},{"id":7,"is_plugin":false,"tab_position":0,"tab_name":"work","pane_columns":100,"pane_x":0,"title":"zsh","terminal_command":"/bin/zsh"},{"id":8,"is_plugin":false,"tab_position":1,"tab_name":"background","pane_columns":40,"pane_x":0,"title":"rimz-sidebar","terminal_command":"rimz"}]\n'
    exit 0 ;;
esac
exit 1
"#,
    );
    let listing = room
        .backend(&shim)
        .authoritative_pane_listing(
            "rimz-test",
            None,
            Some(&room.workspace_id),
            Duration::from_secs(1),
        )
        .expect("authoritative listing");
    assert_eq!(listing.panes.len(), 3, "cache-only pane stays absent");
    let plugin = listing
        .panes
        .iter()
        .find(|pane| pane.is_plugin && pane.id == 7)
        .expect("plugin");
    assert_eq!(plugin.pane_command.as_deref(), Some("plugin-command"));
    assert_eq!(plugin.pane_cwd.as_deref(), Some("/plugin"));
    assert_eq!(plugin.pane_pid, Some(909));
    let active = listing
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 7)
        .expect("active");
    assert_eq!((active.pane_columns, active.pane_x), (Some(100), Some(0)));
    assert_eq!(active.pane_command.as_deref(), Some("vim"));
    assert_eq!(active.pane_pid, Some(707));
    assert_eq!(
        active.pane_cwd.as_deref(),
        Some(room.project_root.path().to_string_lossy().as_ref())
    );
    let background = listing
        .panes
        .iter()
        .find(|pane| pane.id == 8)
        .expect("background");
    assert_eq!(
        (background.pane_columns, background.pane_x),
        (Some(40), Some(0))
    );
    assert_eq!(background.pane_command.as_deref(), Some("rimz-sidebar"));
    assert_eq!(listing.focused_pane, None, "pane roster is not focus truth");
    assert!(listing.produced_at_ms >= unix_now_ms().saturating_sub(1_000));
    assert!(shim_log(&temp).contains("action list-panes --all --json"));
}

#[cfg(unix)]
#[test]
fn authoritative_list_panes_falls_back_unless_required() {
    let room = TestRoom::new();
    room.write_cache(
        unix_now_ms(),
        Some(8),
        None,
        vec![PaneTopologyPane {
            tab_name: Some("fallback".to_owned()),
            pane_command: Some("zsh".to_owned()),
            ..terminal_pane(8, 1, 80, 0, "zsh")
        }],
    );
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
exit 1
"#,
    );
    let backend = room.backend(&shim);
    let listing = backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-test".to_owned()),
            workspace_id: Some(room.workspace_id.clone()),
            consistency: PaneReadConsistency::PreferAuthoritative,
            ..Default::default()
        })
        .expect("optional authoritative read falls back");
    assert_eq!(listing.panes[0].pane_id.raw(), "terminal_8");
    let err = backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-test".to_owned()),
            workspace_id: Some(room.workspace_id.clone()),
            consistency: PaneReadConsistency::RequireAuthoritative,
            ..Default::default()
        })
        .expect_err("required authoritative read propagates failure");
    assert!(matches!(err, crate::mux::MuxErr::Command { .. }));
    assert_eq!(
        command_count(&shim_log(&temp), "action list-panes --all --json"),
        2
    );
}

#[cfg(unix)]
#[test]
fn list_panes_fails_fast_when_session_absent() {
    let room = TestRoom::new();
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "list-sessions" ]; then exit 0; fi
exit 1
"#,
    );
    let err = room
        .backend(&shim)
        .list_panes(PaneListOptions {
            session_name: Some("rimz-dead".to_owned()),
            workspace_id: Some(room.workspace_id.clone()),
            command_timeout: Some(Duration::from_secs(5)),
            ..Default::default()
        })
        .expect_err("absent session");
    assert!(
        matches!(err, crate::mux::MuxErr::SessionNotFound { session } if session == "rimz-dead")
    );
    let log = shim_log(&temp);
    assert!(
        log.contains("list-sessions --no-formatting") && !log.contains("rimz:dump_topology"),
        "{log}"
    );
}

#[cfg(unix)]
fn assert_stepwise_width(
    name: &str,
    initial: u64,
    step: i64,
    view: u64,
    target: u16,
    direction: &str,
    calls: usize,
) {
    let room = TestRoom::new();
    room.write_cache(
        unix_now_ms().saturating_sub(1_000),
        Some(8),
        None,
        vec![
            terminal_pane(8, 1, initial, 0, "rimz-sidebar"),
            terminal_pane(9, 1, view - initial, initial, "zsh"),
        ],
    );
    let script = format!(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; state="$dir/resize-count"; attempts="$dir/resize-attempts"
printf '%s\n' "$*" >> "$log"
if [ "$1" = "list-sessions" ]; then printf 'rimz-test [Created 1s ago]\n'; exit 0; fi
case " $* " in
  *" --name rimz:dump_topology "*)
    count=$(cat "$state" 2>/dev/null || printf 0); cols=$(({initial} + count * {step})); work=$(({view} - cols))
    now=$(perl -MTime::HiRes=time -e 'printf "%d\n", time()*1000')
    printf '{{"session_name":"rimz-test","produced_at_ms":%s,"focused_pane":8,"panes":[{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":%s}},{{"id":9,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":%s,"pane_columns":%s}}]}}\n' "$now" "$cols" "$cols" "$work" > "{cache}"
    exit 0 ;;
  *" action resize {direction} right --pane-id terminal_8 "*)
    attempt=$(cat "$attempts" 2>/dev/null || printf 0); attempt=$((attempt + 1)); printf '%s\n' "$attempt" > "$attempts"
    if [ "$attempt" -eq 1 ]; then exit 1; fi
    count=$(cat "$state" 2>/dev/null || printf 0); printf '%s\n' "$((count + 1))" > "$state"; sleep 0.01; exit 0 ;;
esac
exit 0
"#,
        cache = room.runtime.root.join("pane-topology.json").display(),
    );
    let (temp, shim) = zellij_shim(&script);
    let backend = room.backend(&shim);
    let target_cols = std::num::NonZeroU16::new(target).expect("target");
    let view_cols = std::num::NonZeroU16::new(u16::try_from(view).expect("test view"))
        .expect("nonzero test view");
    let width = WidthSyncOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: room.workspace_id.clone(),
        target: crate::mux::SidebarTarget {
            share: crate::mux::WidthPermille::from_cols(target_cols, view_cols),
            max_cols: target_cols,
            pinned: true,
        },
    };
    let (floor, resized) = backend.converge_sidebar_widths_stepwise(&width, 1, 8, None);
    assert!(resized, "{name}: expected resize");
    let log = shim_log(&temp);
    assert_eq!(
        command_count(
            &log,
            &format!("action resize {direction} right --pane-id terminal_8")
        ),
        calls,
        "{name}:\n{log}"
    );
    let final_cols = backend
        .topology_panes_for_workspace(
            "rimz-test",
            &room.workspace_id,
            floor,
            crate::sidebar::timing::RECONCILE_LIST_TIMEOUT,
        )
        .expect("final topology")
        .into_iter()
        .find(|pane| pane.is_terminal() && pane.id == 8)
        .and_then(|pane| pane.pane_columns)
        .expect("sidebar columns");
    let target = u64::from(width.target.cols(Some(view_cols.get())).get());
    assert!(
        !crate::mux::width::sidebar_width_off_spec(
            final_cols,
            target,
            crate::mux::width::zellij_resize_stop_step_cols(view)
        ),
        "{name}: final width {final_cols}"
    );
}

#[cfg(unix)]
#[test]
fn stepwise_sidebar_width_converges_across_supported_steps() {
    for (name, initial, step, view, target, direction, calls) in [
        ("shrink", 90, -1, 360, 72, "decrease", 2),
        ("grow", 40, 19, 380, 72, "increase", 3),
        ("full-step-below", 53, 10, 213, 64, "increase", 3),
    ] {
        assert_stepwise_width(name, initial, step, view, target, direction, calls);
    }
}

#[cfg(unix)]
fn assert_stepwise_park(
    name: &str,
    widths: &[u64],
    target: u16,
    expected_increase: usize,
    expected_decrease: usize,
) {
    let initial = widths[0];
    let last = widths[widths.len() - 1];
    let geometry_cases = widths
        .iter()
        .enumerate()
        .map(|(count, width)| format!("      {count}) cols={width} ;;\n"))
        .collect::<String>();
    let room = TestRoom::new();
    room.write_cache(
        unix_now_ms().saturating_sub(1_000),
        Some(8),
        None,
        vec![
            terminal_pane(8, 1, initial, 0, "rimz-sidebar"),
            terminal_pane(9, 1, 213 - initial, initial, "zsh"),
        ],
    );
    let script = format!(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; state="$dir/resize-count"
printf '%s\n' "$*" >> "$log"
if [ "$1" = "list-sessions" ]; then printf 'rimz-test [Created 1s ago]\n'; exit 0; fi
case " $* " in
  *" --name rimz:dump_topology "*)
    count=$(cat "$state" 2>/dev/null || printf 0)
    case "$count" in
{geometry_cases}      *) cols={last} ;;
    esac
    work=$((213 - cols))
    now=$(perl -MTime::HiRes=time -e 'printf "%d\n", time()*1000')
    printf '{{"session_name":"rimz-test","produced_at_ms":%s,"focused_pane":8,"panes":[{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":%s}},{{"id":9,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":%s,"pane_columns":%s}}]}}\n' "$now" "$cols" "$cols" "$work" > "{cache}"
    exit 0 ;;
  *" action resize increase right --pane-id terminal_8 "*)
    count=$(cat "$state" 2>/dev/null || printf 0); printf '%s\n' "$((count + 1))" > "$state"; sleep 0.01; exit 0 ;;
  *" action resize decrease right --pane-id terminal_8 "*)
    count=$(cat "$state" 2>/dev/null || printf 0); printf '%s\n' "$((count + 1))" > "$state"; sleep 0.01; exit 0 ;;
esac
exit 0
"#,
        cache = room.runtime.root.join("pane-topology.json").display(),
    );
    let (temp, shim) = zellij_shim(&script);
    let backend = room.backend(&shim);
    let target_cols = std::num::NonZeroU16::new(target).expect("target");
    let view_cols = std::num::NonZeroU16::new(213).expect("view");
    let width = WidthSyncOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: room.workspace_id.clone(),
        target: crate::mux::SidebarTarget {
            share: crate::mux::WidthPermille::from_cols(target_cols, view_cols),
            max_cols: target_cols,
            pinned: true,
        },
    };

    assert!(
        backend
            .converge_sidebar_widths_stepwise(&width, 1, 8, None)
            .1,
        "{name}: expected resize",
    );
    let log = shim_log(&temp);
    assert_eq!(
        command_count(&log, "action resize increase right --pane-id terminal_8"),
        expected_increase,
        "{name}:\n{log}",
    );
    assert_eq!(
        command_count(&log, "action resize decrease right --pane-id terminal_8"),
        expected_decrease,
        "{name}:\n{log}",
    );
    let action_count = std::fs::read_to_string(temp.path().join("resize-count"))
        .expect("resize count")
        .trim()
        .parse::<usize>()
        .expect("numeric resize count");
    let final_cols = widths.get(action_count).copied().unwrap_or(last);
    let target = u64::from(width.target.cols(Some(view_cols.get())).get());
    assert!(
        !crate::mux::width::sidebar_width_off_spec(
            final_cols,
            target,
            crate::mux::width::zellij_resize_stop_step_cols(213),
        ),
        "{name}: final width {final_cols}",
    );
    assert!(
        final_cols >= target && final_cols.saturating_sub(11) < target,
        "{name}: {final_cols} is not the smallest reachable width above {target}",
    );
}

#[cfg(unix)]
#[test]
fn stepwise_sidebar_width_parks_at_the_smallest_reachable_width_above_target() {
    assert_stepwise_park("default-between-lattice-points", &[53, 64], 54, 1, 0);
    assert_stepwise_park("grow", &[48, 59, 70], 64, 2, 0);
    assert_stepwise_park("shrink", &[82, 71], 64, 0, 1);
    assert_stepwise_park(
        "coarse-step undershoot reverses once",
        &[53, 76, 63, 64],
        64,
        2,
        1,
    );
}

#[cfg(unix)]
#[test]
fn stepwise_sidebar_width_uses_authoritative_geometry_over_fresh_stale_cache() {
    let room = TestRoom::new();
    room.write_cache(
        unix_now_ms().saturating_sub(1_000),
        Some(8),
        None,
        vec![
            terminal_pane(8, 1, 171, 0, "rimz-sidebar"),
            terminal_pane(9, 1, 209, 171, "zsh"),
        ],
    );
    let script = format!(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; state="$dir/resize-count"
printf '%s\n' "$*" >> "$log"
if [ "$1" = "list-sessions" ]; then printf 'rimz-test [Created 1s ago]\n'; exit 0; fi
case " $* " in
  *" action list-panes --all --json "*)
    count=$(cat "$state" 2>/dev/null || printf 0); cols=$((171 - count * 19)); if [ "$cols" -lt 72 ]; then cols=72; fi
    now=$(perl -MTime::HiRes=time -e 'printf "%d\n", time()*1000')
    printf '{{"session_name":"rimz-test","produced_at_ms":%s,"focused_pane":8,"panes":[{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":72}},{{"id":9,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":72,"pane_columns":308}}]}}\n' "$now" > "{cache}"
    printf '[{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":%s}},{{"id":9,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":%s,"pane_columns":%s}}]\n' "$cols" "$cols" "$((380 - cols))"; exit 0 ;;
  *" action resize decrease right --pane-id terminal_8 "*)
    count=$(cat "$state" 2>/dev/null || printf 0); printf '%s\n' "$((count + 1))" > "$state"; exit 0 ;;
esac
exit 0
"#,
        cache = room.runtime.root.join("pane-topology.json").display(),
    );
    let (temp, shim) = zellij_shim(&script);
    let backend = room.backend(&shim);
    let target_cols = std::num::NonZeroU16::new(72).expect("target");
    let view_cols = std::num::NonZeroU16::new(380).expect("view");
    let width = WidthSyncOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: room.workspace_id.clone(),
        target: crate::mux::SidebarTarget {
            share: crate::mux::WidthPermille::from_cols(target_cols, view_cols),
            max_cols: target_cols,
            pinned: true,
        },
    };
    assert!(
        backend
            .converge_sidebar_widths_stepwise(&width, 1, 8, None)
            .1
    );
    let log = shim_log(&temp);
    assert_eq!(
        command_count(&log, "action resize decrease right --pane-id terminal_8"),
        5,
        "{log}"
    );
    let final_cols = backend
        .authoritative_pane_listing(
            "rimz-test",
            None,
            Some(&room.workspace_id),
            crate::sidebar::timing::RECONCILE_LIST_TIMEOUT,
        )
        .expect("final listing")
        .panes
        .into_iter()
        .find(|pane| pane.is_terminal() && pane.id == 8)
        .and_then(|pane| pane.pane_columns)
        .expect("sidebar columns");
    assert!(!crate::mux::width::sidebar_width_off_spec(
        final_cols,
        72,
        crate::mux::width::zellij_resize_stop_step_cols(380)
    ));
}

#[cfg(unix)]
#[test]
fn redock_moves_across_every_adjacent_pane_before_resizing() {
    let room = TestRoom::new();
    let mut stale: Vec<_> = (1..=7)
        .map(|id| PaneTopologyPane {
            ..terminal_pane(id, 1, 140, (id - 1) * 140, "zsh")
        })
        .collect();
    stale.push(terminal_pane(8, 1, 140, 980, "rimz-sidebar"));
    room.write_cache(9_999_999_999_999, Some(1), None, stale);
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; moves="$dir/move-count"; published="$dir/published-count"; resized="$dir/resized"
printf '%s\n' "$*" >> "$log"
case " $* " in
  *" action list-panes --all --json "*)
    count=$(cat "$moves" 2>/dev/null || printf 0); if [ "$count" -gt 7 ]; then count=7; fi
    visible=$(cat "$published" 2>/dev/null || printf 0); if [ "$visible" -lt "$count" ]; then printf '%s\n' "$count" > "$published"; fi
    slot=$((7 - visible)); sidebar_x=$((slot * 140)); cols=140; if [ -f "$resized" ]; then cols=72; fi
    printf '['; i=0
    while [ "$i" -lt 7 ]; do
      if [ "$i" -ge "$slot" ]; then x=$(((i + 1) * 140)); else x=$((i * 140)); fi
      if [ "$i" -gt 0 ]; then printf ','; fi
      printf '{"id":%s,"is_plugin":false,"tab_position":1,"pane_columns":140,"pane_x":%s,"title":"zsh"}' "$((i + 1))" "$x"; i=$((i + 1))
    done
    printf ',{"id":8,"is_plugin":false,"tab_position":1,"pane_columns":%s,"pane_x":%s,"title":"rimz-sidebar"}]\n' "$cols" "$sidebar_x"; exit 0 ;;
  *" action move-pane left --pane-id terminal_8 "*) count=$(cat "$moves" 2>/dev/null || printf 0); printf '%s\n' "$((count + 1))" > "$moves"; exit 0 ;;
  *" action resize decrease right --pane-id terminal_8 "*) : > "$resized"; exit 0 ;;
esac
exit 1
"#,
    );
    let backend = room.backend(&shim);
    backend.converge_sidebar_geometry(&room.sidebar_options(1120), 1, 8, Some(0));
    let log = shim_log(&temp);
    let lines: Vec<_> = log.lines().collect();
    let moves: Vec<_> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| line.contains("action move-pane left").then_some(i))
        .collect();
    let resize = lines
        .iter()
        .position(|line| line.contains("action resize decrease right"))
        .expect("resize");
    assert_eq!(moves.len(), 7, "{log}");
    assert!(resize > *moves.last().expect("moves"), "{log}");
    let listing = backend
        .structural_geometry_listing("rimz-test", &room.workspace_id, None)
        .expect("final geometry");
    assert_eq!(
        listing
            .panes
            .iter()
            .find(|pane| pane.id == 8)
            .and_then(|pane| pane.pane_x),
        Some(0)
    );
}

#[cfg(unix)]
#[test]
fn redock_stops_on_authoritative_no_progress() {
    let room = TestRoom::new();
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); printf '%s\n' "$*" >> "$dir/zellij.log"
case " $* " in
  *" action list-panes --all --json "*) printf '[{"id":1,"is_plugin":false,"tab_position":1,"pane_columns":90,"pane_x":0,"title":"zsh"},{"id":2,"is_plugin":false,"tab_position":1,"pane_columns":90,"pane_x":90,"title":"zsh"},{"id":8,"is_plugin":false,"tab_position":1,"pane_columns":90,"pane_x":180,"title":"rimz-sidebar"}]\n'; exit 0 ;;
  *" action move-pane left --pane-id terminal_8 "*) exit 0 ;;
esac
exit 1
"#,
    );
    room.backend(&shim)
        .converge_sidebar_geometry(&room.sidebar_options(270), 1, 8, Some(0));
    let log = shim_log(&temp);
    assert_eq!(command_count(&log, "action move-pane left"), 1, "{log}");
    assert!(!log.contains("action resize"), "{log}");
}

#[cfg(unix)]
#[test]
fn width_nudge_targets_only_named_pane() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); printf '%s\n' "$*" >> "$dir/zellij.log"; exit 0
"#,
    );
    let pane = PaneId::from_parts(crate::MuxName::Zellij, "terminal_8");
    ZellijBackend::with_program_for_test(&shim)
        .nudge_sidebar_width("rimz-test", &pane, 40, 72)
        .expect("nudge");
    let log = shim_log(&temp);
    assert_eq!(
        command_count(&log, "action resize increase right --pane-id terminal_8"),
        1
    );
    assert!(
        !log.contains("list-panes") && !log.contains("list-clients"),
        "{log}"
    );
}

#[cfg(unix)]
#[test]
fn sidebar_add_never_cleans_cross_talk_hint_and_uses_supported_split() {
    let room = TestRoom::new();
    room.write_cache(
        unix_now_ms(),
        Some(7),
        None,
        vec![PaneTopologyPane {
            pane_command: Some("zsh".to_owned()),
            terminal_command: Some("zsh".to_owned()),
            ..terminal_pane(7, 1, 120, 0, "zsh")
        }],
    );
    let script = format!(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; state="$dir/new-pane-count"
printf 'pane=%s args=%s\n' "$ZELLIJ_PANE_ID" "$*" >> "$log"
if [ "$1" = "--version" ]; then printf 'zellij 0.44.3\n'; exit 0; fi
if [ "$1" = "list-sessions" ]; then printf 'rimz-test [Created 1s ago]\n'; exit 0; fi
case " $* " in
  *" --name rimz:dump_topology "*)
    count=$(cat "$state" 2>/dev/null || printf 0)
    now=$(perl -MTime::HiRes=time -e 'printf "%d\n", time()*1000')
    if [ "$count" -ge 2 ]; then
      printf '{{"session_name":"rimz-test","produced_at_ms":%s,"focused_pane":7,"panes":[{{"id":9,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":30}},{{"id":7,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":30,"pane_columns":90}}]}}\n' "$now" > "{cache}"
    else
      printf '{{"session_name":"rimz-test","produced_at_ms":%s,"focused_pane":7,"panes":[{{"id":7,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":0,"pane_columns":90}},{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":90,"pane_columns":30}}]}}\n' "$now" > "{cache}"
    fi
    exit 0 ;;
  *" action list-panes --all --json "*)
    count=$(cat "$state" 2>/dev/null || printf 0)
    if [ "$count" -ge 2 ]; then printf '[{{"id":9,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":30}},{{"id":7,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":30,"pane_columns":90}}]\n';
    elif [ "$count" -ge 1 ]; then printf '[{{"id":7,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":0,"pane_columns":90}},{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":90,"pane_columns":30}}]\n';
    else printf '[{{"id":7,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":0,"pane_columns":120}}]\n'; fi
    exit 0 ;;
  *" action new-pane "*) count=$(cat "$state" 2>/dev/null || printf 0); printf '%s\n' "$((count + 1))" > "$state"; printf 'terminal_7\n'; exit 0 ;;
esac
exit 0
"#,
        cache = room.runtime.root.join("pane-topology.json").display(),
    );
    let (temp, shim) = zellij_shim(&script);
    if let Err(err) = room
        .backend(&shim)
        .add_sidebar_to_tab(&room.sidebar_options(120), 1, Some(0))
    {
        panic!("retry add: {err}\n{}", shim_log(&temp));
    }
    let log = shim_log(&temp);
    let adds: Vec<_> = log
        .lines()
        .filter(|line| line.contains(" action new-pane "))
        .collect();
    assert_eq!(adds.len(), 2, "first misdock must retry:\n{log}");
    assert!(
        adds.iter().all(|line| line.contains("new-pane --tab-id 1")
            && line.contains("--borderless true")
            && !line.contains("--near-current-pane")
            && !line.contains("--direction")),
        "{log}"
    );
    assert!(
        !log.contains("action go-to-tab") && !log.contains("action focus-pane-id"),
        "stable tab targeting must not mutate global focus:\n{log}"
    );
    assert!(
        log.contains("close-pane --pane-id terminal_8"),
        "topology-proven failed add is cleaned:\n{log}"
    );
    assert!(
        !log.contains("close-pane --pane-id terminal_7"),
        "cross-talk hint must stay open:\n{log}"
    );
}

#[cfg(unix)]
#[test]
fn commands_classify_session_not_found_for_zero_and_nonzero_exit() {
    for (name, script) in [
        (
            "zero",
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'zellij 0.44.3\n'; exit 0; fi
printf '\033[32;1mrimz-other\033[m [Created 6m ago]\n'
printf "Session 'missing-room' not found. The following sessions are active:\n" >&2
exit 0
"#,
        ),
        (
            "nonzero",
            r#"#!/bin/sh
if [ "$1" = "--version" ]; then printf 'zellij 0.44.3\n'; exit 0; fi
printf "Session 'missing-room' not found. The following sessions are active:\n" >&2
printf '\033[32;1mrimz-other\033[m [Created 6m ago]\n' >&2
exit 1
"#,
        ),
    ] {
        let (_temp, shim) = zellij_shim(script);
        let err = ZellijBackend::with_program_for_test(&shim)
            .list_tabs("missing-room")
            .expect_err(name);
        assert!(
            matches!(err, crate::mux::MuxErr::SessionNotFound { ref session } if session == "missing-room"),
            "{name}: {err}"
        );
        assert!(!err.to_string().contains("rimz-other"), "{name}: {err}");
    }
}

#[cfg(unix)]
#[test]
fn list_tabs_retries_a_parsed_empty_listing() {
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; count_file="$dir/list-tabs-count"
printf '%s\n' "$*" >> "$log"
count=$(cat "$count_file" 2>/dev/null || printf 0); count=$((count + 1)); printf '%s\n' "$count" > "$count_file"
if [ "$count" -eq 1 ]; then printf '[]\n'; else printf '[{"name":"main"}]\n'; fi
"#,
    );

    let tabs = ZellijBackend::with_program_for_test(&shim)
        .list_tabs("room")
        .expect("retry empty listing");

    assert_eq!(tabs.len(), 1);
    assert_eq!(tabs[0].name, "main");
    let log = shim_log(&temp);
    assert_eq!(command_count(&log, "action list-tabs --json --panes"), 2);
}

#[cfg(unix)]
#[test]
fn new_tab_keeps_layout_until_panes_materialize() {
    let room = TestRoom::new();
    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0"); log="$dir/zellij.log"; tab="$dir/tab-created"; layout_ref="$dir/layout-path"; count_file="$dir/list-tabs-count"
printf '%s\n' "$*" >> "$log"
if [ "$1" = "--version" ]; then printf 'zellij 0.44.3\n'; exit 0; fi
case " $* " in
  *" action new-tab "*) while [ "$#" -gt 0 ]; do if [ "$1" = "--layout" ]; then shift; printf '%s' "$1" > "$layout_ref"; fi; shift; done; : > "$tab"; exit 0 ;;
  *" action list-tabs "*)
    count=$(cat "$count_file" 2>/dev/null || printf 0); count=$((count + 1)); printf '%s\n' "$count" > "$count_file"
    printf '[{"name":"main","selectable_tiled_panes_count":1}'
    if [ -f "$tab" ]; then panes=0; layout=$(cat "$layout_ref" 2>/dev/null || true); if [ "$count" -ge 3 ]; then if [ -n "$layout" ] && [ -f "$layout" ]; then panes=2; else printf 'layout-missing-before-materialized\n' >> "$log"; fi; fi; printf ',{"name":"work","selectable_tiled_panes_count":%s}' "$panes"; fi
    printf ']\n'; exit 0 ;;
esac
exit 0
"#,
    );
    room.backend(&shim)
        .open_tab(&TabOptions {
            title: "work".to_owned(),
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![PaneCmd {
                        argv: vec!["sleep".to_owned(), "600".to_owned()],
                    }],
                    stacked: false,
                }],
            },
            focus: true,
            dock_sidebar: true,
            sidebar: room.sidebar_options(120),
        })
        .expect("open tab");
    let log = shim_log(&temp);
    assert!(
        command_count(&log, "action list-tabs --json --panes") >= 3,
        "{log}"
    );
    assert!(!log.contains("layout-missing-before-materialized"), "{log}");
    assert!(!log.contains("query-tab-names"), "{log}");
    assert_eq!(command_count(&log, "action new-tab "), 1, "{log}");
}

#[test]
fn runtime_dir_pins_full_zellij_env_surface() {
    let runtime = tempfile::TempDir::new().expect("runtime");
    let runtime = runtime.path().to_string_lossy().into_owned();
    let pinned = ZellijBackend::with_runtime_dir(&runtime).cmd();
    let keys = [
        "XDG_RUNTIME_DIR",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "HOME",
        "TMPDIR",
    ];
    for key in keys {
        assert_eq!(pinned.env.get(key), Some(&runtime), "{key}");
    }
    let default = ZellijBackend::default().cmd();
    for key in keys {
        assert!(
            !default.env.contains_key(key),
            "production must inherit {key}"
        );
    }
}

#[test]
fn version_parser_accepts_zellij_output_shapes() {
    assert_eq!(parse_version("zellij 0.41.2"), Some((0, 41, 2)));
    assert_eq!(parse_version("  zellij 1.2.3  \n"), Some((1, 2, 3)));
    assert_eq!(parse_version("zellij 0.44"), Some((0, 44, 0)));
    assert_eq!(parse_version("garbage"), None);
}

#[test]
fn version_serves_the_memoized_probe() {
    let backend = ZellijBackend::default();
    backend
        .version
        .set("zellij 9.9.9".to_owned())
        .expect("fresh cache");
    assert_eq!(backend.version().expect("cached version"), "zellij 9.9.9");
}

#[test]
fn zellij_options_respect_defaults_overrides_and_version_gates() {
    use crate::config::{ZellijClipboard, ZellijForceClose};

    let expected_defaults = expected_option_map(
        "--auto-layout=false --default-mode=locked --disable-session-metadata=true --focus-follows-mouse=false --mouse-click-through=true --osc8-hyperlinks=true --session-serialization=false --stacked-resize=true",
    );
    assert_eq!(
        option_map(&zellij_options_args(
            &ZellijConfig::default(),
            Some((0, 44, 3))
        )),
        expected_defaults
    );
    let mut unknown_defaults = expected_defaults.clone();
    unknown_defaults.remove("--mouse-click-through");
    assert_eq!(
        option_map(&zellij_options_args(&ZellijConfig::default(), None)),
        unknown_defaults
    );

    let mouse = ZellijConfig {
        advanced_mouse_actions: Some(true),
        mouse_hover_effects: Some(false),
        ..ZellijConfig::default()
    };
    for (name, version, gated) in [
        ("unknown", None, false),
        ("0.43.9", Some((0, 43, 9)), false),
        ("0.44.0", Some((0, 44, 0)), true),
        ("0.44.3", Some((0, 44, 3)), true),
    ] {
        let args = zellij_options_args(&mouse, version);
        let map = option_map(&args);
        assert_eq!(map.get("--advanced-mouse-actions"), Some(&"true"), "{name}");
        assert_eq!(
            map.get("--mouse-click-through"),
            gated.then_some(&"true"),
            "{name}"
        );
        assert_eq!(
            map.get("--mouse-hover-effects"),
            gated.then_some(&"false"),
            "{name}"
        );
    }

    let configured = ZellijConfig {
        mouse_mode: Some(false),
        pane_frames: Some(true),
        on_force_close: Some(ZellijForceClose::Quit),
        scroll_buffer_size: Some(200_000),
        show_startup_tips: Some(true),
        show_release_notes: Some(true),
        copy_clipboard: Some(ZellijClipboard::Primary),
        copy_on_select: Some(false),
        support_kitty_keyboard_protocol: Some(false),
        osc8_hyperlinks: false,
        ..ZellijConfig::default()
    };
    assert_eq!(
        option_map(&zellij_options_args(&configured, Some((0, 44, 3)))),
        expected_option_map(
            "--auto-layout=false --copy-clipboard=primary --copy-on-select=false --default-mode=locked --disable-session-metadata=true --focus-follows-mouse=false --mouse-click-through=true --mouse-mode=false --on-force-close=quit --osc8-hyperlinks=false --pane-frames=true --scroll-buffer-size=200000 --session-serialization=false --show-release-notes=true --show-startup-tips=true --stacked-resize=true --support-kitty-keyboard-protocol=false"
        )
    );
}
