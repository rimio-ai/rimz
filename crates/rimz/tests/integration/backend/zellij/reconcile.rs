use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rimz::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};
use rimz::mux::{MuxBackend, SidebarLiveness, SidebarPaneOptions, SidebarWidth};
use rimz::sidebar::heartbeat::SidebarHeartbeat;
use rimz::store::RuntimePaths;
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, Env};

use super::support::*;

#[derive(Debug, PartialEq, Eq)]
struct TerminalState {
    tab_id: u64,
    tab_position: Option<u64>,
    x: u64,
    y: u64,
    columns: u64,
    rows: u64,
    focused: bool,
    title: Option<String>,
}

fn terminal_state(snapshot: &PaneSnapshot) -> BTreeMap<u64, TerminalState> {
    snapshot
        .panes
        .iter()
        .filter(|pane| !pane.is_plugin)
        .map(|pane| {
            (
                pane.id,
                TerminalState {
                    tab_id: pane.tab_id,
                    tab_position: pane.tab_position,
                    x: pane.pane_x,
                    y: pane.pane_y,
                    columns: pane.pane_columns,
                    rows: pane.pane_rows,
                    focused: pane.is_focused,
                    title: pane.title.clone(),
                },
            )
        })
        .collect()
}

/// Upgrade-only reload records an unreachable target as unconverged while
/// preserving every terminal pane, its geometry, and focus. The stale
/// heartbeats model supervisors whose source image vanished before staging was
/// introduced; structural replacement belongs exclusively to `sidebar repair`.
#[test]
fn bare_reload_preserves_stale_sidebar_panes_and_geometry() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("reloadkeep");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let env = Env::new();
    let (_stub_dir, stub) = sidebar_command_stub();
    let layout = write_kdl_layout(cwd.path(), &stub, "reload-keep.kdl", |cwd_kdl, stub_kdl| {
        format!(
            r#"layout {{
    default_tab_template split_direction="vertical" {{
        pane size="25%" name="rimz-sidebar" cwd={cwd_kdl} {{
            command {stub_kdl}
            close_on_exit true
        }}
        children
    }}
    tab name="one" {{
        pane focus=true cwd={cwd_kdl} {{
            command "sleep"
            args "600"
        }}
    }}
    tab name="two" {{
        pane focus=true cwd={cwd_kdl} {{
            command "sleep"
            args "600"
        }}
    }}
    tab name="three" {{
        pane focus=true cwd={cwd_kdl} {{
            command "sleep"
            args "600"
        }}
    }}
}}
"#,
        )
    });
    birth_kdl_session(
        xdg.path(),
        &name,
        cwd.path(),
        &layout,
        "reload preservation",
    );
    let _client = AttachedClient::attach(xdg.path(), &name, 180, 50);
    wait_for_attached_client(xdg.path(), &name);
    wait_for_pane_count(xdg.path(), &name, 6);

    let project_root = PathBuf::from(format!("/tmp/rimz-{name}"));
    let workspace_id = WorkspaceId::from_project_root(&project_root);
    record_known_workspace_session(&env.state_root(), &workspace_id, &project_root, &name);
    assert!(
        rimz::workspace::known_workspaces_under(&rimz::store::paths::workspaces_dir_under(
            &env.state_root()
        ),)
        .expect("known workspaces")
        .iter()
        .any(|workspace| workspace.session_name == name),
        "reload fixture workspace is discoverable",
    );
    assert!(
        rimz::mux::ZellijBackend::with_runtime_dir(xdg.path())
            .list_sessions()
            .expect("list fixture sessions")
            .iter()
            .any(|session| session == &name),
        "reload fixture session is live",
    );
    let runtime = RuntimePaths::under(workspace_id.clone(), xdg.path()).expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let before = expect_list_panes(xdg.path(), &name);
    let before_terminals = terminal_state(&before);
    let mut receivers = Vec::new();
    for pane in before.panes.iter().filter(|pane| pane.is_sidebar()) {
        let socket = runtime.sock_dir.join(format!("reload-{}.sock", pane.id));
        receivers.push(UnixDatagram::bind(&socket).expect("bind stale sidebar socket"));
        let mut heartbeat = SidebarHeartbeat::new(
            workspace_id.clone(),
            SidebarInstanceId::new(),
            MuxName::Zellij,
            name.clone(),
            socket,
            Some(PaneId::from_parts(
                MuxName::Zellij,
                format!("terminal_{}", pane.id),
            )),
        );
        heartbeat.build = Some("vanished-temp-build".to_owned());
        std::fs::write(
            runtime
                .heartbeat_dir
                .join(format!("sidebar.{}.json", pane.id)),
            serde_json::to_vec(&heartbeat).expect("serialize stale heartbeat"),
        )
        .expect("write stale heartbeat");
    }
    assert_eq!(receivers.len(), 3, "fixture has one sidebar in each tab");

    let zellij_trace = TempDir::new().expect("zellij trace tempdir");
    let zellij_log = zellij_trace.path().join("zellij.log");
    let zellij_shim = zellij_trace.path().join("zellij");
    let real_zellij = which::which("zellij").expect("zellij path");
    std::fs::write(
        &zellij_shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            zellij_log.display(),
            real_zellij.display(),
        ),
    )
    .expect("write zellij trace shim");
    std::fs::set_permissions(&zellij_shim, std::fs::Permissions::from_mode(0o755))
        .expect("chmod zellij trace shim");

    let preflight = env
        .rimz()
        .arg("list")
        .env("XDG_RUNTIME_DIR", xdg.path())
        .env("ZELLIJ_SOCKET_DIR", xdg.path().join("zellij"))
        .env("XDG_CACHE_HOME", xdg.path())
        .env("TMPDIR", xdg.path())
        .bounded_output_within(Duration::from_secs(10))
        .expect("list fixture through rimz");
    assert!(
        String::from_utf8_lossy(&preflight.stdout).contains(&name),
        "rimz should discover the fixture before reload; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&preflight.stdout),
        String::from_utf8_lossy(&preflight.stderr),
    );
    let output = env
        .rimz()
        .arg("reload")
        .env("XDG_RUNTIME_DIR", xdg.path())
        .env("ZELLIJ_SOCKET_DIR", xdg.path().join("zellij"))
        .env("XDG_CACHE_HOME", xdg.path())
        .env("TMPDIR", xdg.path())
        .env("RIMZ_ZELLIJ_BIN", &zellij_shim)
        .env("RIMZ_TEST_SKIP_STATS_RELOAD", "1")
        .bounded_output_within(Duration::from_secs(30))
        .expect("run bare reload");
    assert!(
        output.status.success(),
        "bare reload failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("3 sidebars still converging"),
        "stale sidebars should be visible as unconverged: {stdout}",
    );
    let mux_log = std::fs::read_to_string(&zellij_log).expect("read zellij trace log");
    for mutation in ["action new-pane", "action close-pane", "action resize"] {
        assert!(
            !mux_log.contains(mutation),
            "bare reload issued structural mutation `{mutation}`:\n{mux_log}",
        );
    }

    let after = expect_list_panes(xdg.path(), &name);
    assert_eq!(
        terminal_state(&after),
        before_terminals,
        "bare reload must preserve terminal ids, tabs, geometry, and focus",
    );
}

/// An in-place add on a *detached* session is deferred, never attempted:
/// Zellij's screen thread drops a `new-pane` mount when no client is attached
/// while the spawned process keeps running, so the only safe move is to wait
/// for an attached client. Regression test for the reload loop that leaked
/// (and then reaped) one serve pair per run against a detached session.
#[test]
fn reconcile_defers_the_add_on_a_detached_session() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("defer");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    // A detached background session with a working pane and no sidebar.
    create_plain_background_session(xdg.path(), &name, cwd.path(), "60");
    let before = wait_for_pane_count(xdg.path(), &name, 1);
    assert!(
        !before.is_empty(),
        "plain session should have a pane before reconcile: {before:?}",
    );

    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = sidebar_opts(&name, cwd.path(), stub, 120);
    write_topology_cache_from_list_panes(xdg.path(), &opts.workspace_id, &name);
    // A freshly born --create-background session whose only pane is still
    // materializing is the case most prone to reconcile's transient-empty read,
    // so retry until reconcile actually observes the working pane.
    let report = reconcile_until_observed(xdg.path(), &opts, &SidebarLiveness::default());

    assert_eq!(report.deferred, 1, "the detached session's add is deferred");
    assert_eq!(report.recovered, 0, "nothing is added without a client");
    assert_eq!(report.failed, 0, "a deferral is not a failure");
    // Poll rather than read once: a recovered add would already have tripped the
    // `recovered == 0` assertion above, so here a single empty `list-panes` answer
    // under load would only flake a settled result. `before` polls for the same reason.
    let after = wait_for_pane_count(xdg.path(), &name, 1);
    assert_eq!(after.len(), 1, "no pane was added detached: {after:?}");
    assert_eq!(
        serve_processes_for(&name),
        0,
        "no serve pair leaked for the deferred add",
    );
}
/// A claimed live sidebar sitting off the layout's dock — the residue of the
/// pre-discovery mis-mount at the far right — is converged in place by
/// reconcile across every work column and resized toward the fixed birth
/// width, with every existing process and the renderer pane untouched.
#[test]
fn reconcile_redocks_an_off_spec_claimed_sidebar() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("redock");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    // A background session with a deterministic far-right sidebar, matching
    // the shape a raced live add used to leave behind in a wide work tab.
    let (_stub_dir, stub) = sidebar_command_stub();
    let layout = write_kdl_layout(
        cwd.path(),
        &stub,
        "right-sidebar.kdl",
        |cwd_kdl, stub_kdl| {
            let work = (1..=6)
                .map(|index| {
                    format!(
                        r#"        pane name="redock-work-{index}" cwd={cwd_kdl} {{
            command "sleep"
            args "600"
            start_suspended false
            close_on_exit true
        }}
"#,
                    )
                })
                .collect::<String>();
            format!(
                r#"layout {{
    pane split_direction="vertical" {{
{work}
        pane name="rimz-sidebar" cwd={cwd_kdl} {{
            command {stub_kdl}
            start_suspended false
            close_on_exit true
        }}
    }}
}}
"#,
            )
        },
    );
    birth_kdl_session(xdg_dir.path(), &name, cwd.path(), &layout, "right-sidebar");
    let initial = wait_for_pane_count(xdg_dir.path(), &name, 7);
    assert_eq!(
        initial.len(),
        7,
        "layout should birth a sidebar and six work panes: {initial:?}",
    );

    // A wide client: the 50% mis-mount must exceed the `max_cols` cap (72) to
    // trip the tolerant width trigger — at 240 columns it lands at ~120.
    let _client = AttachedClient::attach(xdg_dir.path(), &name, 240, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);
    let before = raw_sidebar_pane(&xdg, &name);
    let sidebar_id = before.id;
    let before_work = work_pane_geometry(&xdg, &name);
    let before_work_ids: BTreeSet<u64> = before_work.iter().map(|pane| pane.id).collect();
    assert_eq!(
        before_work.len(),
        6,
        "wide fixture work panes: {before_work:?}"
    );
    assert!(
        before.pane_x > 0,
        "the recreated mis-mount starts off the left column: {before:?}",
    );
    let focused_work_id = before_work
        .iter()
        .max_by_key(|pane| pane.x)
        .map(|pane| pane.id)
        .expect("rightmost work pane");
    focus_nonplugin_pane_until(
        &xdg,
        &name,
        before.tab_id,
        focused_work_id,
        "rightmost work pane before redock",
    );

    let project_root = std::env::temp_dir();
    let opts = reconcile_opts(
        &name,
        "/tmp/rimz-redock",
        &project_root,
        &project_root,
        stub,
        240,
    );
    let liveness = claimed_liveness(sidebar_id);
    write_topology_cache_from_list_panes(&xdg, &opts.workspace_id, &name);
    let _mirror = topology_cache_mirror(&xdg, &opts.workspace_id, &name);
    let report = reconcile_until_converged(&xdg, &opts, &liveness);

    assert_eq!(report.redocked, 1, "the off-spec claimed sidebar converges");
    assert_eq!(report.closed, 0, "the renderer's pane is never closed");
    assert_eq!(report.recovered, 0, "nothing needed adding");
    assert_eq!(report.failed, 0);
    assert_eq!(report.misdocked, 0);
    assert_sidebar_is_left_docked(&xdg, &name);
    wait_for_sidebar_width_at_most(&xdg, &name, u64::from(opts.birth_size.cols.get()));
    assert_sidebar_identity(
        &xdg,
        &name,
        sidebar_id,
        "the same pane survived the move — the renderer was never replaced",
    );
    let after_work_ids: BTreeSet<u64> = work_pane_geometry(&xdg, &name)
        .into_iter()
        .map(|pane| pane.id)
        .collect();
    assert_eq!(
        after_work_ids, before_work_ids,
        "redock preserves every work pane",
    );
    assert_eq!(
        wait_for_focused_nonplugin_id_in_tab(&xdg, &name, before.tab_id, focused_work_id),
        Some(focused_work_id),
        "redock restores the original focused work pane",
    );
}
/// A claimed sidebar can sit at `x=0` while still not being a full-height left
/// column: a work pane spans the whole tab below it. Reconcile detects that
/// nested row, preserves the running renderer, and moves the work panes into a
/// right-side stack so the sidebar owns the full left column.
#[test]
fn reconcile_repairs_a_nested_sidebar_into_a_full_height_left_column() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("nested");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_command_stub();
    let layout = write_kdl_layout(cwd.path(), &stub, "nested.kdl", |cwd_kdl, stub_kdl| {
        format!(
            r#"layout {{
    pane split_direction="horizontal" {{
        pane split_direction="vertical" {{
            pane name="rimz-sidebar" cwd={cwd_kdl} {{
                command {stub_kdl}
                start_suspended false
                close_on_exit true
            }}
            pane name="nested-left-work" cwd={cwd_kdl} {{
                command "sleep"
                args "600"
                start_suspended false
                close_on_exit true
            }}
        }}
        pane name="nested-original-work" cwd={cwd_kdl} {{
            command "sleep"
            args "600"
            start_suspended false
            close_on_exit true
        }}
    }}
}}
"#,
        )
    });
    birth_kdl_session(xdg_dir.path(), &name, cwd.path(), &layout, "nested");
    let initial = wait_for_pane_count(xdg_dir.path(), &name, 3);
    assert_eq!(
        initial.len(),
        3,
        "layout should birth a sidebar and two work panes: {initial:?}",
    );

    let _client = AttachedClient::attach(xdg_dir.path(), &name, 240, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);

    let before = raw_sidebar_pane(&xdg, &name);
    let sidebar_id = before.id;
    assert_eq!(
        before.pane_x, 0,
        "the nested sidebar starts in the left row band: {before:?}",
    );
    let sidebar_cols = before.pane_columns;
    let before_work = work_pane_geometry(&xdg, &name);
    let before_right_xs: BTreeSet<u64> = before_work
        .iter()
        .filter(|pane| pane.x >= sidebar_cols)
        .map(|pane| pane.x)
        .collect();
    assert!(
        before_work.len() == 2
            && before_work.iter().any(|pane| pane.x == 0)
            && before_right_xs.len() == 1,
        "fixture should start as a repairable nested sidebar: \
         sidebar={before:?}, work={before_work:?}",
    );
    let original_id = before_work
        .iter()
        .find(|pane| pane.x >= sidebar_cols)
        .map(|pane| pane.id)
        .expect("right-side work pane");
    let tab_id = before.tab_id;
    focus_nonplugin_pane_until(
        &xdg,
        &name,
        tab_id,
        original_id,
        "original work pane before reconcile",
    );

    let opts = reconcile_opts(&name, "/tmp/rimz-nested", cwd.path(), cwd.path(), stub, 160);
    let liveness = claimed_liveness(sidebar_id);
    write_topology_cache_from_list_panes(&xdg, &opts.workspace_id, &name);
    let _mirror = topology_cache_mirror(&xdg, &opts.workspace_id, &name);
    let report = reconcile_until_converged(&xdg, &opts, &liveness);

    assert_eq!(report.redocked, 1, "the nested sidebar converges");
    assert_eq!(report.closed, 0, "geometry repair is not duplicate cleanup");
    assert_eq!(report.failed, 0);
    assert_eq!(report.misdocked, 0);
    assert_sidebar_is_left_docked(&xdg, &name);
    assert_sidebar_identity(
        &xdg,
        &name,
        sidebar_id,
        "the renderer pane survives the nested-row repair",
    );
    let focused = wait_for_focused_non_sidebar_title_in_tab(&xdg, &name, tab_id)
        .unwrap_or_else(|| panic!("tab {tab_id} has no focused terminal pane"));
    assert_ne!(
        focused, "rimz-sidebar",
        "in-place nested repair focuses the sidebar; focus must land on the work area",
    );
}
/// A nested sidebar beside a user-made multi-column work layout is detected but
/// not rewritten: stacking every work pane would preserve processes while
/// collapsing the user's right-side columns.
#[test]
fn reconcile_reports_nested_multicolumn_sidebar_without_stacking_work_area() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("nestedwide");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    let (_stub_dir, stub) = sidebar_command_stub();
    let layout = write_kdl_layout(cwd.path(), &stub, "nested-wide.kdl", |cwd_kdl, stub_kdl| {
        format!(
            r#"layout {{
    pane split_direction="horizontal" {{
        pane split_direction="vertical" {{
            pane name="rimz-sidebar" cwd={cwd_kdl} {{
                command {stub_kdl}
                start_suspended false
                close_on_exit true
            }}
            pane cwd={cwd_kdl} {{
                command "sleep"
                args "600"
                start_suspended false
                close_on_exit true
            }}
            pane cwd={cwd_kdl} {{
                command "sleep"
                args "600"
                start_suspended false
                close_on_exit true
            }}
        }}
        pane cwd={cwd_kdl} {{
            command "sleep"
            args "600"
            start_suspended false
            close_on_exit true
        }}
    }}
}}
"#,
        )
    });
    birth_kdl_session(xdg_dir.path(), &name, cwd.path(), &layout, "nested-wide");
    let initial = wait_for_pane_count(xdg_dir.path(), &name, 4);
    assert_eq!(
        initial.len(),
        4,
        "layout should birth four panes: {initial:?}"
    );

    let _client = AttachedClient::attach(xdg_dir.path(), &name, 240, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);

    let before_sidebar = raw_sidebar_pane(&xdg, &name);
    let sidebar_id = before_sidebar.id;
    let before_sidebar_cols = before_sidebar.pane_columns;
    let before_work = work_pane_geometry(&xdg, &name);
    let before_ids: BTreeSet<u64> = before_work.iter().map(|pane| pane.id).collect();
    let before_right_xs: BTreeSet<u64> = before_work
        .iter()
        .filter(|pane| pane.x >= before_sidebar_cols)
        .map(|pane| pane.x)
        .collect();
    assert!(
        before_work.iter().any(|pane| pane.x == 0) && before_right_xs.len() >= 2,
        "fixture should start as a nested sidebar with a multi-column work area: \
         sidebar={before_sidebar:?}, work={before_work:?}",
    );

    let opts = reconcile_opts(
        &name,
        "/tmp/rimz-nestedwide",
        cwd.path(),
        cwd.path(),
        stub,
        240,
    );
    let liveness = claimed_liveness(sidebar_id);
    write_topology_cache_from_list_panes(&xdg, &opts.workspace_id, &name);
    let _mirror = topology_cache_mirror(&xdg, &opts.workspace_id, &name);
    let report = reconcile_until_converged(&xdg, &opts, &liveness);

    assert_eq!(
        report.misdocked, 1,
        "the nested sidebar is reported for operator visibility",
    );
    assert_eq!(
        report.redocked, 0,
        "the arbitrary work layout is not repaired"
    );
    assert_eq!(report.closed, 0, "the claimed renderer pane is preserved");
    assert_eq!(report.failed, 0);

    let after_sidebar = raw_sidebar_pane(&xdg, &name);
    let after_sidebar_cols = after_sidebar.pane_columns;
    let after_work = work_pane_geometry(&xdg, &name);
    let after_ids: BTreeSet<u64> = after_work.iter().map(|pane| pane.id).collect();
    let after_right_xs: BTreeSet<u64> = after_work
        .iter()
        .filter(|pane| pane.x >= after_sidebar_cols)
        .map(|pane| pane.x)
        .collect();
    assert_eq!(after_ids, before_ids, "work panes are not replaced");
    assert!(
        after_work.iter().any(|pane| pane.x == 0) && after_right_xs.len() >= 2,
        "reconcile must not collapse the user's multi-column work area: \
         sidebar={after_sidebar:?}, work={after_work:?}",
    );
    assert_sidebar_identity(&xdg, &name, sidebar_id, "the renderer pane is not rebuilt");
}
/// A missing sidebar in a wide tab is docked after best-effort focus placement,
/// independent of where the attached client began. Reconcile keeps every work
/// pane alive and restores the user's original focus after docking.
#[test]
fn reconcile_add_docks_sidebar_in_wide_tab() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("wideadd");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let layout = write_kdl_layout(cwd.path(), &stub, "wide-add.kdl", |cwd_kdl, _| {
        let work = (1..=6)
            .map(|index| {
                format!(
                    r#"        pane name="wide-add-work-{index}" cwd={cwd_kdl} {{
            command "sleep"
            args "600"
            start_suspended false
            close_on_exit true
        }}
"#,
                )
            })
            .collect::<String>();
        format!(
            r#"layout {{
    pane split_direction="vertical" {{
{work}
    }}
}}
"#,
        )
    });
    birth_kdl_session(xdg_dir.path(), &name, cwd.path(), &layout, "wide add");
    let initial = wait_for_pane_count(xdg_dir.path(), &name, 6);
    assert_eq!(initial.len(), 6, "fixture should birth six work panes");

    let _client = AttachedClient::attach(xdg_dir.path(), &name, 360, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);
    let before = work_pane_geometry(&xdg, &name);
    let before_ids: BTreeSet<u64> = before.iter().map(|pane| pane.id).collect();
    let leftmost_x = before
        .iter()
        .map(|pane| pane.x)
        .min()
        .expect("leftmost work pane");
    let focused_work_id = before
        .iter()
        .max_by_key(|pane| pane.x)
        .map(|pane| pane.id)
        .expect("rightmost work pane");
    assert!(
        before
            .iter()
            .find(|pane| pane.id == focused_work_id)
            .is_some_and(|pane| pane.x > leftmost_x),
        "fixture focus must be away from the left edge: {before:?}",
    );
    let tab_id = expect_list_panes(&xdg, &name)
        .panes
        .iter()
        .find(|pane| pane.id == focused_work_id)
        .map(|pane| pane.tab_id)
        .expect("work pane tab");
    focus_nonplugin_pane_until(
        &xdg,
        &name,
        tab_id,
        focused_work_id,
        "rightmost work pane before sidebar add",
    );

    let opts = reconcile_opts(
        &name,
        "/tmp/rimz-wideadd",
        cwd.path(),
        cwd.path(),
        stub,
        360,
    );
    write_topology_cache_from_list_panes(&xdg, &opts.workspace_id, &name);
    let _mirror = topology_cache_mirror(&xdg, &opts.workspace_id, &name);
    let report = reconcile_until_converged(&xdg, &opts, &SidebarLiveness::default());

    assert_eq!(
        report.recovered, 1,
        "the missing sidebar is added once: {report:?}",
    );
    assert_eq!(report.closed, 0);
    assert_eq!(report.failed, 0);
    assert_eq!(report.misdocked, 0);
    assert_sidebar_is_left_docked(&xdg, &name);
    let after_ids: BTreeSet<u64> = work_pane_geometry(&xdg, &name)
        .into_iter()
        .map(|pane| pane.id)
        .collect();
    assert_eq!(after_ids, before_ids, "every work pane survives the add");
    assert_eq!(
        wait_for_focused_nonplugin_id_in_tab(&xdg, &name, tab_id, focused_work_id),
        Some(focused_work_id),
        "the original focused work pane is restored",
    );
}
/// Adding a sidebar to a tab whose work panes are already row-stacked used to
/// birth the sidebar into only one row. The verified add path now repairs that
/// nested shape before reporting success.
#[test]
fn reconcile_add_ends_docked_in_a_row_stacked_tab() {
    require_zellij!();

    let xdg_dir = scoped_runtime_dir();
    let name = unique_session_name("rowadd");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg_dir.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");

    create_plain_background_session(xdg_dir.path(), &name, cwd.path(), "600");
    wait_for_pane_count(xdg_dir.path(), &name, 1);

    let _client = AttachedClient::attach(xdg_dir.path(), &name, 160, 60);
    let xdg = xdg_dir.path().to_path_buf();
    wait_for_attached_client(&xdg, &name);
    let down = scoped_zellij(&xdg)
        .args([
            "--session",
            &name,
            "action",
            "new-pane",
            "--direction",
            "down",
            "--",
            "sleep",
            "600",
        ])
        .bounded_output()
        .expect("new-pane down");
    assert!(
        down.status.success(),
        "new-pane down failed: {}",
        String::from_utf8_lossy(&down.stderr),
    );
    wait_for_pane_count(&xdg, &name, 2);
    let (_stub_dir, stub) = sidebar_command_stub();

    let opts = reconcile_opts(&name, "/tmp/rimz-rowadd", cwd.path(), cwd.path(), stub, 160);
    write_topology_cache_from_list_panes(&xdg, &opts.workspace_id, &name);
    let _mirror = topology_cache_mirror(&xdg, &opts.workspace_id, &name);
    let report = reconcile_until_converged(&xdg, &opts, &rimz::mux::SidebarLiveness::default());
    let after = expect_list_panes(&xdg, &name);

    assert_eq!(
        report.recovered, 1,
        "the missing sidebar is added: report={report:?}, panes={after:?}",
    );
    assert_eq!(report.failed, 0);
    assert_eq!(report.misdocked, 0);
    assert_sidebar_is_left_docked(&xdg, &name);
}

fn write_kdl_layout(
    cwd: &Path,
    stub: &Path,
    file_name: &str,
    render: impl FnOnce(&str, &str) -> String,
) -> PathBuf {
    let stub_kdl = serde_json::to_string(&stub.to_string_lossy()).expect("stub kdl string");
    let cwd_kdl = serde_json::to_string(&cwd.to_string_lossy()).expect("cwd kdl string");
    let layout = cwd.join(file_name);
    std::fs::write(&layout, render(&cwd_kdl, &stub_kdl)).expect("write kdl layout");
    layout
}

fn birth_kdl_session(xdg: &Path, name: &str, cwd: &Path, layout: &Path, label: &str) {
    let created = scoped_zellij(xdg)
        .args(["attach", "--create-background", name, "options"])
        .arg("--default-cwd")
        .arg(cwd)
        .arg("--default-layout")
        .arg(layout)
        .bounded_status()
        .unwrap_or_else(|err| panic!("create {label} session failed to run: {err}"));
    assert!(created.success(), "create-background failed for {name}");
}

fn reconcile_opts(
    name: &str,
    workspace_root: &str,
    project_root: &Path,
    cwd: &Path,
    stub: PathBuf,
    detected_cols: u16,
) -> SidebarPaneOptions {
    SidebarPaneOptions {
        session_name: name.to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new(workspace_root)),
        project_root: project_root.to_path_buf(),
        extra_env: BTreeMap::from([(
            "RIMZ_TEST_ASSUME_SIDEBAR_HEARTBEAT".to_owned(),
            "1".to_owned(),
        )]),
        cwd: cwd.to_path_buf(),
        width: SidebarWidth::default(),
        birth_size: SidebarWidth::default().birth_size(Some(detected_cols)),
        detected_view_size: None,
        width_override: None,
        rimz_bin: stub,
        replace_existing: false,
        pristine_birth: false,
        config: rimz::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    }
}

fn claimed_liveness(raw_sidebar_id: u64) -> SidebarLiveness {
    let mut liveness = SidebarLiveness::default();
    liveness.claimed_panes.insert(PaneId::from_parts(
        MuxName::Zellij,
        format!("terminal_{raw_sidebar_id}"),
    ));
    liveness
}

fn assert_sidebar_identity(xdg: &Path, name: &str, sidebar_id: u64, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let after = raw_sidebar_pane(xdg, name);
        let observed = Some(after.id);
        if observed == Some(sidebar_id) || Instant::now() >= deadline {
            assert_eq!(observed, Some(sidebar_id), "{message}: {after:?}");
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_sidebar_width_at_most(xdg: &Path, name: &str, max_cols: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let sidebar = raw_sidebar_pane(xdg, name);
        let cols = sidebar.pane_columns;
        if cols <= max_cols {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "redock should shrink the sidebar to the canonical width: \
                 got {cols}, want <= {max_cols}",
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}
