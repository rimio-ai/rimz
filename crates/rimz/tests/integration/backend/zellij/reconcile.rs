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
    title: Option<String>,
}

fn wait_for_stable_terminal_state(
    xdg: &Path,
    session: &str,
    stable_for: Duration,
) -> BTreeMap<u64, TerminalState> {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let mut candidate = None;
    let mut unchanged_since = None;
    let mut last_error = String::new();
    loop {
        match list_panes(xdg, session).map(|snapshot| terminal_state(&snapshot)) {
            Ok(state) => {
                last_error.clear();
                if candidate.as_ref() == Some(&state) {
                    if unchanged_since.is_some_and(|since: Instant| since.elapsed() >= stable_for) {
                        return state;
                    }
                } else {
                    candidate = Some(state);
                    unchanged_since = Some(Instant::now());
                }
            }
            Err(err) => {
                last_error = err;
                candidate = None;
                unchanged_since = None;
            }
        }
        assert!(
            Instant::now() < deadline,
            "terminal state for {session} did not stabilize; candidate: {candidate:?}; last error: {last_error}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
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

    let room = LiveZellijSession::new("reloadkeep");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    let env = Env::new();
    let (_stub_dir, stub) = sidebar_stub_alive_for(120);
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
    birth_kdl_session(&room, cwd.path(), &layout, "reload preservation");
    let client = AttachedClient::attach(&room, 180, 50);
    wait_for_pane_count(xdg, &name, 6);

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
    let backend = rimz::mux::ZellijBackend::with_runtime_dir(xdg);
    assert!(
        wait_for_live_session(&backend, &name)
            .iter()
            .any(|session| session == &name),
        "reload fixture session is live",
    );
    let runtime = RuntimePaths::under(workspace_id.clone(), xdg).expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    // A client can register before the background layout has adopted its PTY
    // size, so record a settled baseline rather than a transient birth frame.
    let before_terminals = wait_for_stable_terminal_state(xdg, &name, Duration::from_millis(500));
    let before = expect_list_panes(xdg, &name);
    let backend = rimz::mux::ZellijBackend::with_runtime_dir(xdg);
    let before_client_view = client.view().viewed_panes;
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
        .env("XDG_RUNTIME_DIR", xdg)
        .env("ZELLIJ_SOCKET_DIR", xdg.join("zellij"))
        .env("XDG_CACHE_HOME", xdg)
        .env("TMPDIR", xdg)
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
        .env("XDG_RUNTIME_DIR", xdg)
        .env("ZELLIJ_SOCKET_DIR", xdg.join("zellij"))
        .env("XDG_CACHE_HOME", xdg)
        .env("TMPDIR", xdg)
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

    let after_terminals = poll_until(
        Duration::from_secs(10),
        || list_panes(xdg, &name).map(|snapshot| terminal_state(&snapshot)),
        |state| state == &before_terminals,
        "terminal geometry to settle after bare reload",
    );
    assert_eq!(
        after_terminals, before_terminals,
        "bare reload must preserve terminal ids, tabs, and geometry",
    );
    let after_client_view = poll_until(
        Duration::from_secs(10),
        || observe_client_view(&backend, &name).map(|view| view.viewed_panes),
        |viewed| viewed == &before_client_view,
        "client view to settle after bare reload",
    );
    assert_eq!(after_client_view, before_client_view);
}

/// An in-place add on a *detached* session is deferred, never attempted:
/// Zellij's screen thread drops a `new-pane` mount when no client is attached
/// while the spawned process keeps running, so the only safe move is to wait
/// for an attached client. Regression test for the reload loop that leaked
/// (and then reaped) one serve pair per run against a detached session.
#[test]
fn reconcile_defers_the_add_on_a_detached_session() {
    require_zellij!();

    let room = LiveZellijSession::new("defer");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");

    // A detached background session with a working pane and no sidebar.
    room.create_plain_background(cwd.path(), "60");

    let (_stub_dir, stub) = sidebar_command_stub();
    let opts = sidebar_opts(&name, cwd.path(), stub, 120);
    write_topology_cache_from_list_panes(xdg, &opts.workspace_id, &name);
    // Cache publication owns readiness for the freshly born working pane, so
    // reconcile consumes that accepted snapshot once.
    let report = room
        .backend()
        .reconcile_sidebars(&opts, &SidebarLiveness::default())
        .expect("reconcile_sidebars");

    assert_eq!(report.deferred, 1, "the detached session's add is deferred");
    assert_eq!(report.recovered, 0, "nothing is added without a client");
    assert_eq!(report.failed, 0, "a deferral is not a failure");
    // Poll rather than read once: a recovered add would already have tripped the
    // `recovered == 0` assertion above, so here a single empty `list-panes` answer
    // under load would only flake a settled result.
    let after = wait_for_pane_count(xdg, &name, 1);
    assert_eq!(after.len(), 1, "no pane was added detached: {after:?}");
    assert_eq!(
        serve_processes_for(&name).expect("scan sidebar serve processes"),
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

    let room = LiveZellijSession::new("redock");
    let name = room.name().to_owned();
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
    birth_kdl_session(&room, cwd.path(), &layout, "right-sidebar");
    let initial = wait_for_pane_count(room.path(), &name, 7);
    assert_eq!(
        initial.len(),
        7,
        "layout should birth a sidebar and six work panes: {initial:?}",
    );

    // A wide client: the 50% mis-mount must exceed the `max_cols` cap (72) to
    // trip the tolerant width trigger — at 240 columns it lands at ~120.
    let mut client = AttachedClient::attach(&room, 240, 60);
    let xdg = room.path().to_path_buf();
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
    let focused_work = PaneId::from_parts(MuxName::Zellij, format!("terminal_{focused_work_id}"));
    client.press_alt_until('l', &focused_work, "rightmost work pane before redock");

    let project_root = std::env::temp_dir();
    let view_cols = 240;
    let opts = reconcile_opts(
        &name,
        "/tmp/rimz-redock",
        &project_root,
        &project_root,
        stub,
        view_cols,
    );
    let mut liveness = claimed_liveness(sidebar_id);
    liveness.topology_floor_ms = Some(0);
    write_topology_cache_from_list_panes(&xdg, &opts.workspace_id, &name);
    let _mirror = topology_cache_mirror(&xdg, &opts.workspace_id, &name);
    let report = reconcile_until_converged(&xdg, &opts, &liveness);

    assert_eq!(report.redocked, 1, "the off-spec claimed sidebar converges");
    assert_eq!(report.closed, 0, "the renderer's pane is never closed");
    assert_eq!(report.recovered, 0, "nothing needed adding");
    assert_eq!(report.failed, 0);
    assert_eq!(report.misdocked, 0);
    assert_sidebar_is_left_docked(&xdg, &name);
    wait_for_sidebar_width_at_most(
        &xdg,
        &name,
        u64::from(opts.target.cols(Some(view_cols)).get()),
    );
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
    client.assert_input_reaches(&focused_work, "restored work pane after redock");
}
/// A claimed sidebar can sit at `x=0` while still not being a full-height left
/// column: a work pane spans the whole tab below it. Reconcile detects that
/// nested row, preserves the running renderer, and moves the work panes into a
/// right-side stack so the sidebar owns the full left column.
#[test]
fn reconcile_repairs_a_nested_sidebar_into_a_full_height_left_column() {
    require_zellij!();

    let room = LiveZellijSession::new("nested");
    let name = room.name().to_owned();
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
    birth_kdl_session(&room, cwd.path(), &layout, "nested");
    let initial = wait_for_pane_count(room.path(), &name, 3);
    assert_eq!(
        initial.len(),
        3,
        "layout should birth a sidebar and two work panes: {initial:?}",
    );

    let mut client = AttachedClient::attach(&room, 240, 60);
    let xdg = room.path().to_path_buf();

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
    let original_pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{original_id}"));
    client.press_alt_until('l', &original_pane, "original work pane before reconcile");

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
    client.press_alt_until(
        'l',
        &original_pane,
        "original work pane after nested repair",
    );
    client.assert_input_reaches(
        &original_pane,
        "restored original work pane after nested repair",
    );
}
/// A nested sidebar beside a user-made multi-column work layout is detected but
/// not rewritten: stacking every work pane would preserve processes while
/// collapsing the user's right-side columns.
#[test]
fn reconcile_reports_nested_multicolumn_sidebar_without_stacking_work_area() {
    require_zellij!();

    let room = LiveZellijSession::new("nestedwide");
    let name = room.name().to_owned();
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
    birth_kdl_session(&room, cwd.path(), &layout, "nested-wide");
    let initial = wait_for_pane_count(room.path(), &name, 4);
    assert_eq!(
        initial.len(),
        4,
        "layout should birth four panes: {initial:?}"
    );

    let _client = AttachedClient::attach(&room, 240, 60);
    let xdg = room.path().to_path_buf();

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
/// A missing sidebar in a wide tab is docked while keeping every work pane
/// alive and restoring the user's focused work pane after docking.
#[test]
fn reconcile_add_docks_sidebar_in_wide_tab() {
    require_zellij!();

    let room = LiveZellijSession::new("wideadd");
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_stub_alive_for(120);
    let layout = write_kdl_layout(cwd.path(), &stub, "wide-add.kdl", |cwd_kdl, _| {
        let work = (1..=6)
            .map(|index| {
                let focus = if index == 6 { " focus=true" } else { "" };
                format!(
                    r#"        pane name="wide-add-work-{index}"{focus} cwd={cwd_kdl} {{
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
    birth_kdl_session(&room, cwd.path(), &layout, "wide add");
    let initial = wait_for_pane_count(room.path(), &name, 6);
    assert_eq!(initial.len(), 6, "fixture should birth six work panes");

    let mut client = AttachedClient::attach(&room, 360, 60);
    let xdg = room.path().to_path_buf();
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
    let focused_work = PaneId::from_parts(MuxName::Zellij, format!("terminal_{focused_work_id}"));
    client.press_alt_until('l', &focused_work, "rightmost work pane before sidebar add");

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
    let after_reconcile = expect_list_panes(&xdg, &name);

    assert_eq!(
        report.recovered, 1,
        "the missing sidebar is added once: report={report:?}, panes={after_reconcile:?}",
    );
    assert_eq!(
        report.closed, 0,
        "report={report:?}, panes={after_reconcile:?}"
    );
    assert_eq!(
        report.failed, 0,
        "report={report:?}, panes={after_reconcile:?}"
    );
    assert_eq!(
        report.misdocked, 0,
        "report={report:?}, panes={after_reconcile:?}"
    );
    assert_sidebar_is_left_docked(&xdg, &name);
    let after_ids: BTreeSet<u64> = work_pane_geometry(&xdg, &name)
        .into_iter()
        .map(|pane| pane.id)
        .collect();
    assert_eq!(after_ids, before_ids, "every work pane survives the add");
    client.assert_input_reaches(&focused_work, "restored work pane after sidebar add");
}
/// Adding a sidebar to a tab whose work panes are already row-stacked used to
/// birth the sidebar into only one row. The verified add path now repairs that
/// nested shape before reporting success.
#[test]
fn reconcile_add_ends_docked_in_a_row_stacked_tab() {
    require_zellij!();

    let room = LiveZellijSession::new("rowadd");
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");

    room.create_plain_background(cwd.path(), "600");
    wait_for_pane_count(room.path(), &name, 1);

    let _client = AttachedClient::attach(&room, 160, 60);
    let xdg = room.path().to_path_buf();
    let down = room
        .command()
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

fn birth_kdl_session(session: &LiveZellijSession, cwd: &Path, layout: &Path, label: &str) {
    let created = session
        .command()
        .args(["attach", "--create-background", session.name(), "options"])
        .arg("--default-cwd")
        .arg(cwd)
        .arg("--default-layout")
        .arg(layout)
        .bounded_status()
        .unwrap_or_else(|err| panic!("create {label} session failed to run: {err}"));
    assert!(
        created.success(),
        "create-background failed for {}",
        session.name()
    );
    session.wait_until_ready();
}

fn reconcile_opts(
    name: &str,
    workspace_root: &str,
    project_root: &Path,
    cwd: &Path,
    stub: PathBuf,
    detected_cols: u16,
) -> SidebarPaneOptions {
    let width = SidebarWidth::default();
    let view_cols = std::num::NonZeroU16::new(detected_cols).expect("nonzero test view");
    let requested_cols = std::num::NonZeroU16::new(
        u16::try_from(width.target_cols(u64::from(detected_cols))).expect("test target"),
    )
    .expect("nonzero test width");
    let share = rimz::mux::WidthPermille::from_cols(requested_cols, view_cols);
    let target = rimz::mux::SidebarTarget {
        share,
        max_cols: width.max_cols,
        pinned: false,
    };
    SidebarPaneOptions {
        session_name: name.to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new(workspace_root)),
        project_root: project_root.to_path_buf(),
        extra_env: BTreeMap::from([(
            "RIMZ_TEST_ASSUME_SIDEBAR_HEARTBEAT".to_owned(),
            "1".to_owned(),
        )]),
        cwd: cwd.to_path_buf(),
        target,
        detected_view_size: None,
        rimz_bin: stub,
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
    let after = poll_until(
        Duration::from_secs(15),
        || {
            list_panes(xdg, name)?
                .sidebar()
                .cloned()
                .ok_or_else(|| format!("rimz-sidebar pane missing in {name}"))
        },
        |sidebar| sidebar.id == sidebar_id,
        &format!("sidebar {sidebar_id} to retain its identity"),
    );
    assert_eq!(after.id, sidebar_id, "{message}: {after:?}");
}

fn wait_for_sidebar_width_at_most(xdg: &Path, name: &str, max_cols: u64) {
    poll_until(
        Duration::from_secs(15),
        || {
            list_panes(xdg, name)?
                .sidebar()
                .cloned()
                .ok_or_else(|| format!("rimz-sidebar pane missing in {name}"))
        },
        |sidebar| sidebar.pane_columns <= max_cols,
        &format!("sidebar width <= {max_cols} columns after redock"),
    );
}
