use super::*;

#[cfg(unix)]
fn zellij_shim(script: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::TempDir::new().expect("tempdir");
    let shim = temp.path().join("zellij");
    let mut file = std::fs::File::create(&shim).expect("create shim");
    file.write_all(script.as_bytes()).expect("write shim");
    let mut perms = file.metadata().expect("shim metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).expect("chmod shim");
    drop(file);
    (temp, shim)
}

#[cfg(unix)]
#[test]
fn split_pane_spells_requested_direction() {
    use crate::mux::{MuxBackend, SplitDirection, SplitPaneOptions};

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
exit 0
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);

    for direction in [SplitDirection::Right, SplitDirection::Down] {
        backend
            .split_pane(SplitPaneOptions {
                direction,
                focus: true,
                ..Default::default()
            })
            .expect("split_pane");
    }

    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read shim log");
    assert!(
        log.contains("action new-pane --direction right"),
        "right split must be explicit:\n{log}",
    );
    assert!(
        log.contains("action new-pane --direction down"),
        "down split must be explicit:\n{log}",
    );
}

#[cfg(unix)]
#[test]
fn list_panes_uses_fresh_topology_without_zellij_action() {
    use crate::ids::WorkspaceId;
    use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane, TopologyClients};
    use crate::mux::{MuxBackend, PaneListOptions};
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::sidebar::timing::unix_now_ms;
    use crate::store::paths::RuntimePaths;

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
fi
exit 0
"#,
    );
    let runtime_root = tempfile::TempDir::new().expect("runtime tempdir");
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    let workspace_id = WorkspaceId::from_project_root(&project_root);
    let runtime = RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    write_pane_topology_cache(
        &runtime,
        &PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: unix_now_ms(),
            writer: None,
            focused_pane: Some(7),
            clients: Some(TopologyClients {
                human_clients: 2,
                viewed_panes: vec![7],
            }),
            panes: vec![PaneTopologyPane {
                id: 7,
                is_plugin: false,
                is_held: false,
                exited: false,
                is_suppressed: false,
                is_floating: false,
                is_focused: true,
                tab_position: 0,
                tab_name: Some("work".to_owned()),
                pane_columns: Some(100),
                pane_x: Some(0),
                title: Some("zsh".to_owned()),
                pane_command: Some("zsh".to_owned()),
                pane_cwd: Some(project_root.to_string_lossy().into_owned()),
                terminal_command: None,
            }],
        },
    )
    .expect("write topology cache");

    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, runtime_root.path())
        .with_presence_plugin_for_test(&shim);
    let listing = backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-test".to_owned()),
            workspace_id: Some(workspace_id),
            ..Default::default()
        })
        .expect("list panes from topology");

    assert_eq!(listing.panes.len(), 1);
    assert_eq!(listing.panes[0].pane_id.raw(), "terminal_7");
    assert_eq!(listing.panes[0].view_id.as_deref(), Some("tab_0"));
    assert_eq!(listing.panes[0].command.as_deref(), Some("zsh"));
    assert_eq!(listing.panes[0].spawn_command, None);
    assert_eq!(
        listing.panes[0].cwd.as_deref(),
        Some(project_root.to_string_lossy().as_ref()),
    );
    let client_view = listing.client_view.expect("topology carries client view");
    assert_eq!(client_view.presence.human_clients, 2);
    assert_eq!(client_view.presence.last_input_ms, None);
    assert_eq!(
        client_view.viewed_panes,
        vec![listing.panes[0].pane_id.clone()]
    );
    let log = std::fs::read_to_string(temp.path().join("zellij.log")).unwrap_or_default();
    assert!(
        !log.contains("action list-panes")
            && !log.contains("action list-clients")
            && !log.contains("rimz:dump_topology"),
        "fresh topology should avoid zellij actions:\n{log}",
    );
}

#[cfg(unix)]
#[test]
fn list_panes_trusts_fresh_topology_without_structural_floor() {
    use crate::ids::WorkspaceId;
    use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};
    use crate::mux::{MuxBackend, PaneListOptions};
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::sidebar::timing::unix_now_ms;
    use crate::store::paths::RuntimePaths;

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
exit 1
"#,
    );
    let runtime_root = tempfile::TempDir::new().expect("runtime tempdir");
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    let workspace_id = WorkspaceId::from_project_root(&project_root);
    let runtime = RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    let floor = unix_now_ms();
    write_pane_topology_cache(
        &runtime,
        &PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: floor.saturating_sub(1),
            writer: None,
            focused_pane: Some(7),
            clients: None,
            panes: vec![PaneTopologyPane {
                id: 7,
                is_plugin: false,
                is_held: false,
                exited: false,
                is_suppressed: false,
                is_floating: false,
                is_focused: true,
                tab_position: 0,
                tab_name: Some("work".to_owned()),
                pane_columns: Some(100),
                pane_x: Some(0),
                title: Some("zsh".to_owned()),
                pane_command: Some("zsh".to_owned()),
                pane_cwd: Some(project_root.to_string_lossy().into_owned()),
                terminal_command: None,
            }],
        },
    )
    .expect("write topology cache");

    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, runtime_root.path())
        .with_presence_plugin_for_test(&shim);
    let listing = backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-test".to_owned()),
            workspace_id: Some(workspace_id.clone()),
            min_topology_produced_at_ms: None,
            ..Default::default()
        })
        .expect("fresh topology without structural floor is usable");
    assert_eq!(listing.panes.len(), 1);

    backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-test".to_owned()),
            workspace_id: Some(workspace_id),
            min_topology_produced_at_ms: Some(floor),
            command_timeout: Some(Duration::from_millis(1)),
            ..Default::default()
        })
        .expect_err("explicit repair floor rejects pre-floor topology");
}

#[cfg(unix)]
#[test]
fn authoritative_list_panes_enriches_matching_cache_geometry_only() {
    use crate::ids::WorkspaceId;
    use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::sidebar::timing::unix_now_ms;
    use crate::store::paths::RuntimePaths;

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "--session" ] && [ "$3" = "action" ] && [ "$4" = "list-panes" ]; then
  printf '[{"id":7,"is_plugin":false,"is_focused":true,"tab_position":0,"tab_name":"work","pane_columns":100,"pane_x":0,"title":"zsh","terminal_command":"/bin/zsh"},{"id":8,"is_plugin":false,"tab_position":1,"tab_name":"background","title":"rimz-sidebar","terminal_command":"rimz"}]\n'
  exit 0
fi
exit 1
"#,
    );
    let runtime_root = tempfile::TempDir::new().expect("runtime tempdir");
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    let workspace_id = WorkspaceId::from_project_root(&project_root);
    let runtime = RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    write_pane_topology_cache(
        &runtime,
        &PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: 1,
            writer: None,
            focused_pane: None,
            clients: None,
            panes: vec![
                PaneTopologyPane {
                    id: 7,
                    is_plugin: false,
                    is_held: false,
                    exited: false,
                    is_suppressed: false,
                    is_floating: false,
                    is_focused: false,
                    tab_position: 0,
                    tab_name: Some("old".to_owned()),
                    pane_columns: Some(999),
                    pane_x: Some(999),
                    title: None,
                    pane_command: Some("vim".to_owned()),
                    pane_cwd: Some(project_root.to_string_lossy().into_owned()),
                    terminal_command: None,
                },
                PaneTopologyPane {
                    id: 8,
                    is_plugin: false,
                    is_held: false,
                    exited: false,
                    is_suppressed: false,
                    is_floating: false,
                    is_focused: false,
                    tab_position: 1,
                    tab_name: Some("background".to_owned()),
                    pane_columns: Some(40),
                    pane_x: Some(0),
                    title: Some("rimz-sidebar".to_owned()),
                    pane_command: Some("rimz-sidebar".to_owned()),
                    pane_cwd: None,
                    terminal_command: Some("rimz".to_owned()),
                },
                PaneTopologyPane {
                    id: 9,
                    is_plugin: false,
                    is_held: false,
                    exited: false,
                    is_suppressed: false,
                    is_floating: false,
                    is_focused: false,
                    tab_position: 2,
                    tab_name: Some("gone".to_owned()),
                    pane_columns: Some(50),
                    pane_x: Some(0),
                    title: Some("rimz-sidebar".to_owned()),
                    pane_command: Some("rimz-sidebar".to_owned()),
                    pane_cwd: None,
                    terminal_command: Some("rimz".to_owned()),
                },
            ],
        },
    )
    .expect("write enrichment cache");

    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, runtime_root.path())
        .with_presence_plugin_for_test(&shim);
    let listing = backend
        .authoritative_pane_listing(
            "rimz-test",
            None,
            Some(&workspace_id),
            std::time::Duration::from_secs(1),
        )
        .expect("authoritative list panes");

    assert_eq!(listing.panes.len(), 2, "cache-only panes stay excluded");
    let active = listing
        .panes
        .iter()
        .find(|pane| pane.id == 7)
        .expect("active");
    assert_eq!(active.pane_columns, Some(100));
    assert_eq!(active.pane_x, Some(0));
    assert_eq!(active.pane_command.as_deref(), Some("vim"));
    assert_eq!(
        active.pane_cwd.as_deref(),
        Some(project_root.to_string_lossy().as_ref()),
    );
    let background = listing
        .panes
        .iter()
        .find(|pane| pane.id == 8)
        .expect("background");
    assert_eq!(background.pane_columns, Some(40));
    assert_eq!(background.pane_x, Some(0));
    assert_eq!(background.pane_command.as_deref(), Some("rimz-sidebar"));
    assert_eq!(
        listing.authoritative_focus.as_ref().map(|pane| pane.raw()),
        Some("terminal_7")
    );
    assert!(listing.observed_at_ms >= unix_now_ms().saturating_sub(1_000));
    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read shim log");
    assert!(log.contains("action list-panes --all --json"), "{log}");
}

#[cfg(unix)]
#[test]
fn authoritative_list_panes_falls_back_to_topology_on_failure() {
    use crate::ids::WorkspaceId;
    use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};
    use crate::mux::{MuxBackend, PaneListOptions};
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::sidebar::timing::unix_now_ms;
    use crate::store::paths::RuntimePaths;

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
exit 1
"#,
    );
    let runtime_root = tempfile::TempDir::new().expect("runtime tempdir");
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    let workspace_id = WorkspaceId::from_project_root(&project_root);
    let runtime = RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    write_pane_topology_cache(
        &runtime,
        &PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: unix_now_ms(),
            writer: None,
            focused_pane: Some(8),
            clients: None,
            panes: vec![PaneTopologyPane {
                id: 8,
                is_plugin: false,
                is_held: false,
                exited: false,
                is_suppressed: false,
                is_floating: false,
                is_focused: true,
                tab_position: 1,
                tab_name: Some("fallback".to_owned()),
                pane_columns: Some(80),
                pane_x: Some(0),
                title: Some("zsh".to_owned()),
                pane_command: Some("zsh".to_owned()),
                pane_cwd: None,
                terminal_command: None,
            }],
        },
    )
    .expect("write topology cache");

    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, runtime_root.path())
        .with_presence_plugin_for_test(&shim);
    let listing = backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-test".to_owned()),
            workspace_id: Some(workspace_id),
            authoritative: true,
            ..Default::default()
        })
        .expect("fallback list panes");

    assert_eq!(listing.panes.len(), 1);
    assert_eq!(listing.panes[0].pane_id.raw(), "terminal_8");
    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read shim log");
    assert!(log.contains("action list-panes --all --json"), "{log}");
}

#[cfg(unix)]
#[test]
fn list_panes_fails_fast_when_topology_session_is_absent() {
    use std::time::{Duration, Instant};

    use crate::ids::WorkspaceId;
    use crate::mux::{MuxBackend, MuxErr, PaneListOptions};

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
if [ "$1" = "list-sessions" ]; then
  exit 0
fi
sleep 2
exit 0
"#,
    );
    let runtime_root = tempfile::TempDir::new().expect("runtime tempdir");
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    let workspace_id = WorkspaceId::from_project_root(&project_root);
    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, runtime_root.path())
        .with_presence_plugin_for_test(&shim);

    let started = Instant::now();
    let err = backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-dead".to_owned()),
            workspace_id: Some(workspace_id),
            command_timeout: Some(Duration::from_secs(5)),
            ..Default::default()
        })
        .expect_err("absent session should fail before topology poll");

    assert!(
        started.elapsed() < Duration::from_secs(1),
        "absent sessions should not poll topology until command timeout",
    );
    assert!(matches!(err, MuxErr::SessionNotFound { session } if session == "rimz-dead"));
    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read shim log");
    assert!(
        log.contains("list-sessions --no-formatting") && !log.contains("rimz:dump_topology"),
        "absent session should stop at list-sessions:\n{log}",
    );
}

#[cfg(unix)]
fn assert_resize_sidebar_toward_scenario(
    initial_cols: u64,
    step: i64,
    view_cols: u64,
    direction: &str,
    expected_calls: usize,
) {
    use crate::ids::WorkspaceId;
    use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::sidebar::timing::unix_now_ms;
    use crate::store::paths::RuntimePaths;

    let runtime_root = tempfile::TempDir::new().expect("runtime tempdir");
    let project_root = tempfile::TempDir::new().expect("project tempdir");
    let workspace_id = WorkspaceId::from_project_root(project_root.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    write_pane_topology_cache(
        &runtime,
        &PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: unix_now_ms().saturating_sub(1_000),
            writer: None,
            focused_pane: Some(8),
            clients: None,
            panes: vec![PaneTopologyPane {
                id: 8,
                is_plugin: false,
                is_held: false,
                exited: false,
                is_suppressed: false,
                is_floating: false,
                is_focused: true,
                tab_position: 1,
                tab_name: Some("work".to_owned()),
                pane_columns: Some(initial_cols),
                pane_x: Some(0),
                title: Some("rimz-sidebar".to_owned()),
                pane_command: Some("rimz-sidebar".to_owned()),
                pane_cwd: None,
                terminal_command: Some("rimz".to_owned()),
            }],
        },
    )
    .expect("write ambient topology cache");

    let script = format!(
        r#"#!/bin/sh
dir=$(dirname "$0")
log="$dir/zellij.log"
state="$dir/resize-count"
attempts="$dir/resize-attempts"
cache="{cache}"
printf '%s\n' "$*" >> "$log"

if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

if [ "$1" = "list-sessions" ]; then
  printf 'rimz-test [Created 1s ago]\n'
  exit 0
fi

case " $* " in
  *" --name rimz:dump_topology "*)
    count=$(cat "$state" 2>/dev/null || printf '0')
    cols=$(({initial_cols} + count * {step}))
    now=$(perl -MTime::HiRes=time -e 'printf "%d\n", time()*1000')
    cat > "$cache" <<JSON
{{"session_name":"rimz-test","produced_at_ms":$now,"focused_pane":8,"panes":[{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":$cols,"pane_command":"rimz-sidebar","terminal_command":"rimz"}}]}}
JSON
    exit 0
    ;;
  *" action resize {direction} right --pane-id terminal_8 "*)
    attempt=$(cat "$attempts" 2>/dev/null || printf '0')
    attempt=$((attempt + 1))
    printf '%s\n' "$attempt" > "$attempts"
    if [ "$attempt" -eq 1 ]; then
      exit 1
    fi
    count=$(cat "$state" 2>/dev/null || printf '0')
    count=$((count + 1))
    printf '%s\n' "$count" > "$state"
    sleep 0.01
    exit 0
    ;;
esac

exit 0
"#,
        cache = runtime.root.join("pane-topology.json").display(),
        direction = direction,
        initial_cols = initial_cols,
        step = step,
    );
    let (temp, shim) = zellij_shim(&script);
    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, runtime_root.path())
        .with_presence_plugin_for_test(&shim);
    let width_sync = crate::mux::WidthSyncOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: workspace_id.clone(),
        width: crate::mux::SidebarWidth {
            percent: crate::mux::WidthPercent::Fixed(30),
            max_cols: std::num::NonZeroU16::new(72).expect("non-zero cap"),
        },
        width_override: None,
    };

    let floor =
        backend.resize_sidebar_toward(&width_sync, 1, "terminal_8", initial_cols, view_cols, None);
    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read shim log");
    let resize_calls = log
        .lines()
        .filter(|line| {
            line.contains(&format!(
                " action resize {direction} right --pane-id terminal_8"
            ))
        })
        .count();
    assert_eq!(
        resize_calls, expected_calls,
        "a {direction} should retry its transient error, then stop in-band:\n{log}",
    );

    let final_cols = backend
        .sidebar_cols("rimz-test", &workspace_id, 1, 8, floor)
        .expect("final sidebar columns");
    let target_cols = crate::mux::width::live_target_cols(width_sync.width, None, view_cols);
    assert!(
        !crate::mux::width::sidebar_width_off_spec(
            final_cols,
            target_cols,
            crate::mux::width::zellij_resize_step_cols(view_cols),
        ),
        "final post-action topology should see an in-band width, got {final_cols}",
    );
}

#[cfg(unix)]
#[test]
fn resize_sidebar_toward_retries_shrink_and_stops_in_band() {
    assert_resize_sidebar_toward_scenario(90, -1, 360, "decrease", 10);
}

#[cfg(unix)]
#[test]
fn resize_sidebar_toward_retries_grow_and_stops_in_band() {
    assert_resize_sidebar_toward_scenario(40, 19, 380, "increase", 3);
}

#[cfg(unix)]
#[test]
fn resize_sidebar_toward_repairs_a_full_step_below_target() {
    assert_resize_sidebar_toward_scenario(53, 10, 213, "increase", 2);
}

#[cfg(unix)]
#[test]
fn resize_sidebar_toward_uses_live_geometry_over_newly_stamped_stale_cache() {
    use crate::ids::WorkspaceId;
    use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::sidebar::timing::unix_now_ms;
    use crate::store::paths::RuntimePaths;

    let runtime_root = tempfile::TempDir::new().expect("runtime tempdir");
    let project_root = tempfile::TempDir::new().expect("project tempdir");
    let workspace_id = WorkspaceId::from_project_root(project_root.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    write_pane_topology_cache(
        &runtime,
        &PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: unix_now_ms().saturating_sub(1_000),
            writer: None,
            focused_pane: Some(8),
            clients: None,
            panes: vec![PaneTopologyPane {
                id: 8,
                is_plugin: false,
                is_held: false,
                exited: false,
                is_suppressed: false,
                is_floating: false,
                is_focused: true,
                tab_position: 1,
                tab_name: Some("work".to_owned()),
                pane_columns: Some(171),
                pane_x: Some(0),
                title: Some("rimz-sidebar".to_owned()),
                pane_command: Some("rimz-sidebar".to_owned()),
                pane_cwd: None,
                terminal_command: Some("rimz".to_owned()),
            }],
        },
    )
    .expect("write ambient topology cache");

    let script = format!(
        r#"#!/bin/sh
dir=$(dirname "$0")
log="$dir/zellij.log"
state="$dir/resize-count"
cache="{cache}"
printf '%s\n' "$*" >> "$log"

if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

if [ "$1" = "list-sessions" ]; then
  printf 'rimz-test [Created 1s ago]\n'
  exit 0
fi

case " $* " in
  *" action list-panes --all --json "*)
    count=$(cat "$state" 2>/dev/null || printf '0')
    cols=$((171 - count * 20))
    if [ "$cols" -lt 71 ]; then
      cols=71
    fi
    printf '[{{"id":8,"is_plugin":false,"tab_position":1,"tab_name":"work","title":"rimz-sidebar","pane_x":0,"pane_columns":%s,"terminal_command":"rimz"}}]\n' "$cols"
    exit 0
    ;;
  *" --name rimz:dump_topology "*)
    now=$(perl -MTime::HiRes=time -e 'printf "%d\n", time()*1000')
    cat > "$cache" <<JSON
{{"session_name":"rimz-test","produced_at_ms":$now,"focused_pane":8,"panes":[{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":72,"pane_command":"rimz-sidebar","terminal_command":"rimz"}}]}}
JSON
    exit 0
    ;;
  *" action resize decrease right --pane-id terminal_8 "*)
    count=$(cat "$state" 2>/dev/null || printf '0')
    count=$((count + 1))
    printf '%s\n' "$count" > "$state"
    exit 0
    ;;
esac

exit 0
"#,
        cache = runtime.root.join("pane-topology.json").display(),
    );
    let (temp, shim) = zellij_shim(&script);
    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, runtime_root.path())
        .with_presence_plugin_for_test(&shim);
    let width_sync = crate::mux::WidthSyncOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: workspace_id.clone(),
        width: crate::mux::SidebarWidth {
            percent: crate::mux::WidthPercent::Fixed(30),
            max_cols: std::num::NonZeroU16::new(72).expect("non-zero cap"),
        },
        width_override: None,
    };

    let floor = backend.resize_sidebar_toward(&width_sync, 1, "terminal_8", 171, 380, None);
    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read shim log");
    let resize_calls = log
        .lines()
        .filter(|line| line.contains(" action resize decrease right --pane-id terminal_8"))
        .count();
    assert_eq!(
        resize_calls, 5,
        "stale in-band topology must not stop the live resize loop:\n{log}",
    );

    let final_cols = backend
        .sidebar_cols("rimz-test", &workspace_id, 1, 8, floor)
        .expect("final sidebar columns");
    assert!(
        !crate::mux::width::sidebar_width_off_spec(
            final_cols,
            72,
            crate::mux::width::zellij_resize_step_cols(380),
        ),
        "final live geometry should be in-band, got {final_cols}",
    );
}

#[cfg(unix)]
#[test]
fn add_sidebar_timeout_never_closes_stdout_only_hint() {
    use crate::config::MultiplexerConfig;
    use crate::ids::WorkspaceId;
    use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};
    use crate::mux::{SidebarPaneOptions, SidebarWidth};
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::sidebar::timing::unix_now_ms;
    use crate::store::paths::RuntimePaths;

    let runtime_root = tempfile::TempDir::new().expect("runtime tempdir");
    let project_temp = tempfile::TempDir::new().expect("project tempdir");
    let project_root = project_temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    let workspace_id = WorkspaceId::from_project_root(&project_root);
    let runtime = RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    write_pane_topology_cache(
        &runtime,
        &PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: unix_now_ms(),
            writer: None,
            focused_pane: Some(7),
            clients: None,
            panes: vec![PaneTopologyPane {
                id: 7,
                is_plugin: false,
                is_held: false,
                exited: false,
                is_suppressed: false,
                is_floating: false,
                is_focused: true,
                tab_position: 1,
                tab_name: Some("work".to_owned()),
                pane_columns: Some(120),
                pane_x: Some(0),
                title: Some("zsh".to_owned()),
                pane_command: Some("zsh".to_owned()),
                pane_cwd: None,
                terminal_command: Some("zsh".to_owned()),
            }],
        },
    )
    .expect("write topology cache");

    let script = format!(
        r#"#!/bin/sh
dir=$(dirname "$0")
log="$dir/zellij.log"
state="$dir/new-pane-count"
printf '%s\n' "$*" >> "$log"
cache="{cache}"

if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

case " $* " in
  *" --name rimz:dump_topology "*)
    count=$(cat "$state" 2>/dev/null || printf '0')
    if [ "$count" -ge 2 ]; then
      cat > "$cache" <<'JSON'
{{"session_name":"rimz-test","produced_at_ms":9999999999999,"focused_pane":7,"panes":[{{"id":7,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":30,"pane_columns":90,"pane_command":"zsh","terminal_command":"zsh"}},{{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":30,"pane_command":"rimz-sidebar","terminal_command":"rimz"}}]}}
JSON
    else
      cat > "$cache" <<'JSON'
{{"session_name":"rimz-test","produced_at_ms":9999999999999,"focused_pane":7,"panes":[{{"id":7,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":0,"pane_columns":120,"pane_command":"zsh","terminal_command":"zsh"}}]}}
JSON
    fi
    exit 0
    ;;
  *" action new-pane "*)
    count=$(cat "$state" 2>/dev/null || printf '0')
    count=$((count + 1))
    printf '%s\n' "$count" > "$state"
    if [ "$count" -eq 1 ]; then
      printf 'terminal_7\n'
    else
      printf 'terminal_8\n'
    fi
    exit 0
    ;;
esac
"#,
        cache = runtime.root.join("pane-topology.json").display(),
    );
    let (temp, shim) = zellij_shim(&script);
    let log = temp.path().join("zellij.log");

    let width = SidebarWidth::default();
    let opts = SidebarPaneOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id,
        project_root: project_root.clone(),
        cwd: project_root,
        width,
        birth_size: width.birth_size(Some(120)),
        width_override: None,
        rimz_bin: std::path::PathBuf::from("rimz"),
        replace_existing: false,
        pristine_birth: false,
        config: MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };

    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, runtime_root.path())
        .with_presence_plugin_for_test(&shim);
    assert_eq!(
        backend
            .new_sidebar_pane(&opts, 1)
            .expect("spawn sidebar")
            .as_deref(),
        Some("terminal_7"),
    );
    let log = std::fs::read_to_string(log).expect("read shim log");
    assert!(
        !log.contains("close-pane --pane-id terminal_7"),
        "stdout-only hint for a pre-existing work pane must not be closed:\n{log}",
    );
    assert!(
        log.contains("action new-pane --direction right --name rimz-sidebar --borderless true"),
        "repair-created sidebar panes must be explicitly borderless and position-targeted by focus:\n{log}",
    );
    assert!(
        log.contains("action go-to-tab 2"),
        "tab position 1 targets CLI tab 2:\n{log}"
    );
}

#[cfg(unix)]
#[test]
fn reconcile_targets_tabs_by_position_from_topology_cache() {
    use crate::config::MultiplexerConfig;
    use crate::ids::WorkspaceId;
    use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};
    use crate::mux::{SidebarPaneOptions, SidebarWidth};
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::sidebar::timing::unix_now_ms;
    use crate::store::paths::RuntimePaths;

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
log="$dir/zellij.log"
state="$dir/sidebar-added"
printf '%s\n' "$*" >> "$log"

if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

case " $* " in
  *" action dump-layout "*)
    exit 0
    ;;
  *" action list-clients "*)
    printf '%s\n' 'CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND'
    printf '%s\n' '1 terminal_7 zsh'
    exit 0
    ;;
  *" --name rimz:dump_topology "*)
    cache=$(ls "$XDG_RUNTIME_DIR"/rimz/ws_*/pane-topology.json 2>/dev/null | head -n1)
    if [ -f "$state" ]; then
      cat > "$cache" <<'JSON'
{"session_name":"rimz-test","produced_at_ms":9999999999999,"focused_pane":7,"panes":[{"id":7,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":30,"pane_columns":90,"pane_command":"zsh","terminal_command":"zsh"},{"id":8,"is_plugin":false,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":30,"pane_command":"rimz-sidebar","terminal_command":"rimz"}]}
JSON
    else
      cat > "$cache" <<'JSON'
{"session_name":"rimz-test","produced_at_ms":9999999999999,"focused_pane":7,"panes":[{"id":7,"is_plugin":false,"tab_position":1,"title":"zsh","pane_x":0,"pane_columns":120,"pane_command":"zsh","terminal_command":"zsh"}]}
JSON
    fi
    exit 0
    ;;
  *" action new-pane "*)
    printf '%s\n' "mounted" > "$state"
    printf '%s\n' 'terminal_8'
    exit 0
    ;;
  *" action focus-pane-id "*|*" action move-pane "*|*" action resize "*)
    exit 0
    ;;
esac

exit 0
"#,
    );
    let runtime_root = tempfile::TempDir::new().expect("runtime tempdir");
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    let workspace_id = WorkspaceId::from_project_root(&project_root);
    let runtime = RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    write_pane_topology_cache(
        &runtime,
        &PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: unix_now_ms(),
            writer: None,
            focused_pane: Some(7),
            clients: None,
            panes: vec![PaneTopologyPane {
                id: 7,
                is_plugin: false,
                is_held: false,
                exited: false,
                is_suppressed: false,
                is_floating: false,
                is_focused: true,
                tab_position: 1,
                tab_name: Some("work".to_owned()),
                pane_columns: Some(120),
                pane_x: Some(0),
                title: Some("zsh".to_owned()),
                pane_command: Some("zsh".to_owned()),
                pane_cwd: None,
                terminal_command: Some("zsh".to_owned()),
            }],
        },
    )
    .expect("write topology cache");

    let opts = SidebarPaneOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id,
        project_root: project_root.clone(),
        cwd: project_root,
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(120)),
        width_override: None,
        rimz_bin: std::path::PathBuf::from("rimz"),
        replace_existing: false,
        pristine_birth: false,
        config: MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };

    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, runtime_root.path())
        .with_presence_plugin_for_test(&shim);
    backend
        .new_sidebar_pane(&opts, 1)
        .expect("spawn sidebar in positioned tab");

    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read shim log");
    let new_panes: Vec<&str> = log
        .lines()
        .filter(|line| line.contains(" action new-pane "))
        .collect();
    assert_eq!(new_panes.len(), 1, "one add issued:\n{log}");
    assert!(
        !new_panes[0].contains("--tab-id"),
        "add targets the tab by focusing its position, not by internal tab id:\n{log}",
    );
    assert!(
        log.contains("action go-to-tab 2"),
        "tab position 1 targets CLI tab 2:\n{log}"
    );
}

#[cfg(unix)]
#[test]
fn commands_surface_session_not_found_banner() {
    use crate::mux::MuxErr;

    let (_temp, shim) = zellij_shim(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

printf '\033[32;1mrimz-other\033[m [Created 6m ago]\n'
printf "Session 'missing-room' not found. The following sessions are active:\n" >&2
exit 0
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);

    let err = backend
        .tab_names("missing-room")
        .expect_err("banner should classify as session-not-found");

    assert!(
        matches!(err, MuxErr::SessionNotFound { ref session } if session == "missing-room"),
        "got: {err}",
    );
}

#[cfg(unix)]
#[test]
fn commands_surface_session_not_found_banner_nonzero_exit() {
    use crate::mux::MuxErr;

    let (_temp, shim) = zellij_shim(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

printf "Session 'missing-room' not found. The following sessions are active:\n" >&2
printf '\033[32;1mrimz-other\033[m [Created 6m ago]\n' >&2
exit 1
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);

    let err = backend
        .tab_names("missing-room")
        .expect_err("nonzero banner should classify as session-not-found");
    assert!(
        matches!(err, MuxErr::SessionNotFound { ref session } if session == "missing-room"),
        "got: {err}",
    );
    assert!(
        !err.to_string().contains("rimz-other"),
        "typed error must not leak active session names: {err}",
    );
}

#[cfg(unix)]
#[test]
fn new_tab_confirmation_waits_for_layout_panes() {
    use crate::config::MultiplexerConfig;
    use crate::ids::WorkspaceId;
    use crate::mux::{
        LayoutColumn, LayoutPanes, PaneCmd, SidebarPaneOptions, SidebarWidth, TabOptions,
    };

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
log="$dir/zellij.log"
tab="$dir/tab-created"
layout_ref="$dir/layout-path"
list_count="$dir/list-tabs-count"
printf '%s\n' "$*" >> "$log"

if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

case " $* " in
  *" action dump-layout "*)
    printf 'layout {\n}\n'
    exit 0
    ;;
  *" action query-tab-names "*)
    printf 'main\n'
    if [ -f "$tab" ]; then
      printf 'work\n'
    fi
    exit 0
    ;;
  *" action new-tab "*)
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "--layout" ]; then
        shift
        printf '%s' "$1" > "$layout_ref"
      fi
      shift
    done
    : > "$tab"
    exit 0
    ;;
  *" action list-tabs "*)
    count=$(cat "$list_count" 2>/dev/null || printf '0')
    count=$((count + 1))
    printf '%s\n' "$count" > "$list_count"
    printf '[{"name":"main","selectable_tiled_panes_count":1}'
    if [ -f "$tab" ]; then
      panes=0
      layout=$(cat "$layout_ref" 2>/dev/null || true)
      if [ "$count" -ge 3 ]; then
        if [ -n "$layout" ] && [ -f "$layout" ]; then
          panes=2
        else
          printf '%s\n' 'layout-missing-before-materialized' >> "$log"
        fi
      fi
      printf ',{"name":"work","selectable_tiled_panes_count":%s}' "$panes"
    fi
    printf ']\n'
    exit 0
    ;;
esac
"#,
    );
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    let sidebar = SidebarPaneOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: WorkspaceId::from_project_root(&project_root),
        project_root: project_root.clone(),
        cwd: project_root.clone(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(120)),
        width_override: None,
        rimz_bin: std::path::PathBuf::from("rimz"),
        replace_existing: false,
        pristine_birth: false,
        config: MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };

    let backend = ZellijBackend::with_program_for_test(&shim);
    backend
        .open_tab(&TabOptions {
            session_name: "rimz-test".to_owned(),
            title: "work".to_owned(),
            cwd: project_root,
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
            sidebar,
        })
        .expect("open tab");

    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read shim log");
    let materialize_polls = log
        .lines()
        .filter(|line| line.contains("action list-tabs --json --panes"))
        .count();
    assert!(
        materialize_polls >= 3,
        "new-tab confirmation must wait for pane materialization, got log:\n{log}",
    );
    assert!(
        !log.contains("layout-missing-before-materialized"),
        "the temp layout file must stay alive until panes materialize:\n{log}",
    );
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("action new-tab "))
            .count(),
        1,
        "materialization polling should not create duplicate tabs:\n{log}",
    );
}

#[test]
fn runtime_dir_pins_full_zellij_env_surface() {
    let runtime = tempfile::TempDir::new().expect("runtime tempdir");
    let runtime = runtime.path().to_string_lossy().into_owned();
    let pinned = ZellijBackend::with_runtime_dir(&runtime).cmd();

    for key in [
        "XDG_RUNTIME_DIR",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "HOME",
        "TMPDIR",
    ] {
        assert_eq!(
            pinned.env.get(key),
            Some(&runtime),
            "{key} must point at the test runtime dir",
        );
    }

    let default = ZellijBackend::default().cmd();
    for key in [
        "XDG_RUNTIME_DIR",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "HOME",
        "TMPDIR",
    ] {
        assert!(
            !default.env.contains_key(key),
            "production backend must not override {key}",
        );
    }
}

#[test]
fn version_parser_and_floor_hold() {
    assert_eq!(parse_version("zellij 0.41.2"), Some((0, 41, 2)));
    assert_eq!(parse_version("  zellij 1.2.3  \n"), Some((1, 2, 3)));
    assert_eq!(parse_version("zellij 0.44"), Some((0, 44, 0)));
    assert_eq!(parse_version("garbage"), None);

    assert!((0, 44, 0) >= MIN_ZELLIJ_VERSION);
    assert!((0, 44, 3) >= MIN_ZELLIJ_VERSION);
    assert!((0, 43, 9) < MIN_ZELLIJ_VERSION);
}

#[test]
fn log_classifier_matches_leading_levels_and_panics_only() {
    use crate::mux::logtail::LogSeverity;

    assert_eq!(
        classify_log_line("ERROR failed to decode"),
        Some(LogSeverity::Error)
    );
    assert_eq!(
        classify_log_line("WARN slow client"),
        Some(LogSeverity::Warn)
    );
    assert_eq!(
        classify_log_line("Panic occured: over 1000 consecutive unknown messages"),
        Some(LogSeverity::Panic)
    );
    assert_eq!(
        classify_log_line("INFO later WARN text is not a level"),
        None
    );
    assert_eq!(classify_log_line("WARNING is not WARN token"), None);
}

#[test]
fn version_serves_the_memoized_probe() {
    let backend = ZellijBackend::default();
    backend
        .version
        .set("zellij 9.9.9".to_owned())
        .expect("a fresh instance has not probed yet");
    // The cache is consulted before any probe: the seeded value comes back
    // verbatim — no `zellij --version` fork, no overwrite by a real binary.
    assert_eq!(backend.version().expect("cached version"), "zellij 9.9.9");
}

#[test]
fn option_flags_gate_by_version() {
    assert!(mouse_click_through_args(true, None).is_empty());
    assert!(mouse_click_through_args(true, Some((0, 43, 9))).is_empty());
    assert!(mouse_click_through_args(false, Some((0, 44, 3))).is_empty());
    let expected = vec!["--mouse-click-through".to_owned(), "true".to_owned()];
    assert_eq!(mouse_click_through_args(true, Some((0, 44, 0))), expected);

    let mouse_config = ZellijConfig {
        advanced_mouse_actions: Some(true),
        mouse_hover_effects: Some(false),
        ..ZellijConfig::default()
    };
    let args = zellij_options_args(&mouse_config, Some((0, 42, 9)));
    assert!(
        !args.iter().any(|arg| arg == "--mouse-hover-effects"),
        "Zellij before 0.44 rejects mouse hover effect options"
    );
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--advanced-mouse-actions" && pair[1] == "true"),
        "0.44 is the runtime floor, so advanced mouse options are unconditional"
    );
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--stacked-resize" && pair[1] == "true"),
        "0.44 is the runtime floor, so stacked resize is unconditional"
    );

    let args = zellij_options_args(&mouse_config, Some((0, 41, 9)));
    assert!(
        args.iter().any(|arg| arg == "--stacked-resize"),
        "dead pre-0.44 gates stay deleted"
    );

    let args = zellij_options_args(&mouse_config, Some((0, 43, 0)));
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has("--advanced-mouse-actions", "true"));
    assert!(!args.iter().any(|arg| arg == "--mouse-hover-effects"));

    let args = zellij_options_args(&mouse_config, Some((0, 44, 0)));
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has("--advanced-mouse-actions", "true"));
    assert!(has("--mouse-hover-effects", "false"));
}

#[test]
fn zellij_options_render_defaults_and_unknown_version_floor() {
    let args = zellij_options_args(&ZellijConfig::default(), Some((0, 44, 3)));
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(
        !args.iter().any(|arg| arg == "--mouse-mode"),
        "`--mouse-mode true` disables mouse reporting on Zellij 0.44.3; rely on the default"
    );
    assert!(has("--default-mode", "locked"));
    assert!(has("--mouse-click-through", "true"));
    assert!(has("--focus-follows-mouse", "false"));
    assert!(has("--auto-layout", "false"));
    assert!(has("--stacked-resize", "true"));
    assert!(has("--session-serialization", "false"));
    assert!(has("--disable-session-metadata", "true"));
    assert!(
        !args.iter().any(|arg| arg == "--web-sharing"),
        "normal rooms defer web sharing to the user's Zellij config"
    );
    for flag in [
        "--advanced-mouse-actions",
        "--mouse-hover-effects",
        "--pane-frames",
        "--copy-clipboard",
        "--support-kitty-keyboard-protocol",
        "--osc8-hyperlinks",
    ] {
        assert!(
            !args.iter().any(|arg| arg == flag),
            "unset optional {flag} must defer to Zellij config: {args:?}",
        );
    }

    let unknown = zellij_options_args(&ZellijConfig::default(), None);
    let has_unknown = |flag: &str, value: &str| {
        unknown
            .windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has_unknown("--auto-layout", "false"));
    assert!(has_unknown("--stacked-resize", "true"));
    assert!(has_unknown("--session-serialization", "false"));
    assert!(has_unknown("--disable-session-metadata", "true"));
    assert!(!unknown.iter().any(|arg| arg == "--mouse-click-through"));
    assert!(!unknown.iter().any(|arg| arg == "--advanced-mouse-actions"));
    assert!(!unknown.iter().any(|arg| arg == "--mouse-hover-effects"));
}

#[test]
fn zellij_options_render_configured_optionals() {
    let config = ZellijConfig {
        mouse_mode: Some(false),
        pane_frames: Some(true),
        on_force_close: Some(crate::config::ZellijForceClose::Quit),
        scroll_buffer_size: Some(200_000),
        show_startup_tips: Some(true),
        show_release_notes: Some(true),
        copy_clipboard: Some(crate::config::ZellijClipboard::Primary),
        copy_on_select: Some(false),
        support_kitty_keyboard_protocol: Some(false),
        osc8_hyperlinks: Some(false),
        ..ZellijConfig::default()
    };
    let args = zellij_options_args(&config, Some((0, 44, 3)));
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has("--mouse-mode", "false"));
    assert!(has("--auto-layout", "false"));
    assert!(has("--stacked-resize", "true"));
    assert!(has("--pane-frames", "true"));
    assert!(has("--on-force-close", "quit"));
    assert!(has("--scroll-buffer-size", "200000"));
    assert!(has("--show-startup-tips", "true"));
    assert!(has("--show-release-notes", "true"));
    assert!(has("--copy-clipboard", "primary"));
    assert!(has("--copy-on-select", "false"));
    assert!(has("--support-kitty-keyboard-protocol", "false"));
    assert!(has("--osc8-hyperlinks", "false"));
}
