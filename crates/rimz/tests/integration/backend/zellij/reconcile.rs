use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rimz::disk::paths::RuntimePaths;
use rimz::ids::{MuxName, PaneId, SidebarInstanceId, WorkspaceId};
use rimz::mux::{MuxBackend, SidebarLiveness, SidebarPaneOptions, SidebarWidth};
use rimz::wakeup::heartbeat::SidebarHeartbeat;
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, Env, ZellijNamespace};

use super::support::*;

struct RepairRoom {
    room: LiveZellijSession,
    env: Env,
    _stub: TempDir,
    opts: SidebarPaneOptions,
    proxy: PathBuf,
}

impl RepairRoom {
    fn new(label: &str) -> Self {
        let room = LiveZellijSession::new(label);
        let env = Env::new();
        let (stub_dir, stub) = sidebar_stub_alive_for(120);
        let mut opts = reconcile_opts(
            room.name(),
            "/tmp/unused",
            &env.project_root,
            &env.project_root,
            stub,
            180,
        );
        opts.extra_env.remove("RIMZ_TEST_ASSUME_SIDEBAR_HEARTBEAT");
        opts.workspace_id = env.workspace_id.clone();
        let state =
            rimz::StatePaths::for_project_root_under(&env.project_root, &env.rimz_home()).unwrap();
        opts.runtime = RuntimePaths::for_state_under(&state, room.path());
        opts.runtime.ensure_dirs().unwrap();
        publish_room_bin(&env.rimz_home(), &opts);
        let mut record = rimz::workspace::record::read(&state.workspace_record).unwrap();
        record.rimz_bin = Some(opts.rimz_bin.clone());
        record.rimz_build = Some(rimz::build_id::of_file(&opts.rimz_bin).unwrap());
        rimz::workspace::record::write(&state, &record).unwrap();
        let proxy = stub_dir.path().join("zellij");
        let real = which::which("zellij").unwrap();
        std::fs::write(
            &proxy,
            format!(
                "#!/bin/sh\ncase \" $* \" in *' pipe '*) exit 0 ;; esac\nexec '{}' \"$@\"\n",
                real.display().to_string().replace('\'', "'\\''")
            ),
        )
        .unwrap();
        std::fs::set_permissions(&proxy, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self {
            room,
            env,
            _stub: stub_dir,
            opts,
            proxy,
        }
    }

    fn birth(&self, tabs: &str) {
        let layout = write_kdl_layout(
            &self.env.project_root,
            &self.opts.rimz_bin,
            "repair.kdl",
            |cwd, stub| {
                format!(
                    "layout {{\n{}\n}}\n",
                    tabs.replace("CWD", cwd).replace("STUB", stub)
                )
            },
        );
        // `rimz start` births a room in the RimZ home its repairs later run
        // in, and the reap guard spares a process from another home.
        let mut create = self.room.command();
        create.env("RIMZ_HOME", self.env.rimz_home());
        create_kdl_session(
            &self.room,
            create,
            &self.env.project_root,
            &layout,
            "repair CLI",
        );
    }

    fn repair(&self) -> std::process::Output {
        self.env
            .rimz()
            .args(["sidebar", "repair"])
            .env("XDG_RUNTIME_DIR", self.room.path())
            .env("ZELLIJ_SOCKET_DIR", self.room.path().join("zellij"))
            .env("XDG_CACHE_HOME", self.room.path())
            .env("RIMZ_ZELLIJ_BIN", &self.proxy)
            .bounded_output_within(Duration::from_secs(30))
            .unwrap()
    }

    fn claim(&self, pane: Option<u64>, instance: &SidebarInstanceId) {
        write_repair_claim(&self.opts, pane, instance);
    }
}

fn write_repair_claim(opts: &SidebarPaneOptions, pane: Option<u64>, instance: &SidebarInstanceId) {
    let mut heartbeat = SidebarHeartbeat::new(
        opts.workspace_id.clone(),
        instance.clone(),
        MuxName::Zellij,
        opts.session_name.clone(),
        opts.runtime.sock_dir.join("repair.sock"),
        pane.map(|id| PaneId::from_parts(MuxName::Zellij, format!("terminal_{id}"))),
    );
    heartbeat.build = Some(rimz::build_id::of_file(&opts.rimz_bin).unwrap());
    rimz::disk::atomic::write_temp_then_rename_cache(
        &opts.runtime.sidebar_heartbeat_path(instance),
        &heartbeat,
    )
    .unwrap();
}

fn with_repair_claims<T>(fixture: &RepairRoom, action: impl FnOnce() -> T) -> (T, Option<u64>) {
    use std::sync::atomic::{AtomicBool, Ordering};
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let claims = scope.spawn(|| {
            let mut instances = BTreeMap::new();
            while !stop.load(Ordering::Relaxed) {
                if let Ok(snapshot) = list_panes(fixture.room.path(), fixture.room.name()) {
                    for pane in snapshot.panes.iter().filter(|pane| pane.is_sidebar()) {
                        let instance = instances
                            .entry(pane.id)
                            .or_insert_with(SidebarInstanceId::new);
                        fixture.claim(Some(pane.id), instance);
                    }
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            instances.into_keys().next()
        });
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action));
        stop.store(true, Ordering::Relaxed);
        let first_pane = claims.join().unwrap();
        (
            result.unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            first_pane,
        )
    })
}

#[test]
fn repair_cli_with_a_silent_plugin_adds_from_live_panes_and_exits_zero() {
    require_zellij!();
    let fixture = RepairRoom::new("repairsilent");
    fixture.birth(
        r#"tab name="work" split_direction="horizontal" {
    pane cwd=CWD { command "sleep"; args "600"; }
    pane cwd=CWD { command "sleep"; args "600"; }
}"#,
    );
    let _client = AttachedClient::attach(&fixture.room, 180, 50);
    let (output, _) = with_repair_claims(&fixture, || fixture.repair());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Recovered 1 sidebar in place."), "{stdout}");
    assert!(
        stdout.contains("silent or stale") && stdout.contains("live pane list"),
        "{stdout}"
    );
    assert!(
        output.status.success(),
        "{stdout}\n{}\n{:#?}",
        String::from_utf8_lossy(&output.stderr),
        expect_list_panes(fixture.room.path(), fixture.room.name())
    );
    insta::assert_snapshot!(stdout, @r###"
    Presence plugin silent or stale in 1 session; repair ran from Zellij's live pane list. Run `rimz doctor`; reattach or restart the session to reload the plugin.
    Recovered 1 sidebar in place.
    "###);
    let snapshot = expect_list_panes(fixture.room.path(), fixture.room.name());
    assert_eq!(
        snapshot
            .panes
            .iter()
            .filter(|pane| pane.is_sidebar())
            .count(),
        1
    );
    assert_sidebar_is_left_docked(fixture.room.path(), fixture.room.name());
}

#[test]
fn repair_cli_docks_a_sidebar_beside_a_lone_work_pane() {
    require_zellij!();
    let fixture = RepairRoom::new("repairlone");
    let version = fixture.room.backend().version().expect("version");
    if version
        .split('.')
        .nth(1)
        .and_then(|part| part.parse::<u32>().ok())
        .is_none_or(|minor| minor < 45)
    {
        crate::common::skip("zellij below 0.45");
        return;
    }
    fixture.birth(r#"tab name="work" { pane cwd=CWD { command "sleep"; args "600"; }; }"#);
    let _client = AttachedClient::attach(&fixture.room, 180, 50);
    let before = expect_list_panes(fixture.room.path(), fixture.room.name());
    let work = before
        .panes
        .iter()
        .find(|pane| pane.is_live_terminal())
        .unwrap();
    let (output, _) = with_repair_claims(&fixture, || fixture.repair());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let after = expect_list_panes(fixture.room.path(), fixture.room.name());
    assert!(
        output.status.success(),
        "{stdout}\n{}\n{after:#?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Recovered 1 sidebar in place."), "{stdout}");
    assert!(!stdout.contains("not docked"), "{stdout}");
    let sidebars: Vec<_> = after
        .panes
        .iter()
        .filter(|pane| pane.is_sidebar())
        .collect();
    assert_eq!(sidebars.len(), 1, "{after:#?}");
    let sidebar = sidebars[0];
    assert_eq!(sidebar.pane_x, 0, "{sidebar:?}");
    assert_eq!(sidebar.pane_y, 0, "{sidebar:?}");
    assert_eq!(sidebar.pane_rows, 50, "{sidebar:?}");
    assert!((sidebar.pane_columns as i64 - 45).abs() <= 5, "{sidebar:?}");
    let work_after = after
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == work.id)
        .unwrap();
    assert_eq!(work_after.pane_x, sidebar.pane_columns, "{work_after:?}");
    assert_eq!(work_after.pane_y, work.pane_y, "{work_after:?}");
    assert_eq!(work_after.pane_rows, work.pane_rows, "{work_after:?}");
    assert_eq!(
        work_after.pane_x + work_after.pane_columns,
        180,
        "{work_after:?}"
    );

    let (output, _) = with_repair_claims(&fixture, || fixture.repair());
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let kept = expect_list_panes(fixture.room.path(), fixture.room.name());
    assert_eq!(terminal_state(&kept), terminal_state(&after));
}

#[test]
fn repair_cli_docks_a_sidebar_when_a_lone_work_pane_has_floats_shown() {
    require_zellij!();
    let fixture = RepairRoom::new("repairfloats");
    let version = fixture.room.backend().version().expect("version");
    if version
        .split('.')
        .nth(1)
        .and_then(|part| part.parse::<u32>().ok())
        .is_none_or(|minor| minor < 45)
    {
        crate::common::skip("zellij below 0.45");
        return;
    }
    fixture.birth(r#"tab name="work" { pane cwd=CWD { command "sleep"; args "600"; }; }"#);
    let _client = AttachedClient::attach(&fixture.room, 240, 50);
    let output = ZellijNamespace::command_at(fixture.room.path())
        .args(["--session", fixture.room.name(), "action", "new-pane"])
        .args(["--floating", "--", "sleep", "601"])
        .bounded_output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let before = poll_until(
        SPAWN_TIMEOUT,
        || list_panes(fixture.room.path(), fixture.room.name()),
        |snapshot| {
            snapshot
                .panes
                .iter()
                .any(|pane| !pane.is_plugin && pane.is_floating)
        },
        "the floating work pane",
    );
    let work = before
        .panes
        .iter()
        .find(|pane| pane.is_live_terminal() && !pane.is_floating)
        .unwrap();
    let floating = before
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.is_floating)
        .unwrap();
    let floating_visible = || {
        let output = ZellijNamespace::command_at(fixture.room.path())
            .args(["--session", fixture.room.name(), "action", "list-tabs"])
            .arg("--json")
            .bounded_output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        let tabs: Vec<serde_json::Value> = serde_json::from_slice(&output.stdout).unwrap();
        let tab = tabs
            .iter()
            .find(|tab| tab["tab_id"].as_u64() == Some(work.tab_id))
            .unwrap();
        tab["are_floating_panes_visible"].as_bool().unwrap()
    };
    assert!(
        floating_visible(),
        "repair must start with floating panes shown"
    );

    let (output, _) = with_repair_claims(&fixture, || fixture.repair());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let after = expect_list_panes(fixture.room.path(), fixture.room.name());
    assert!(
        output.status.success(),
        "{stdout}\n{}\n{after:#?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("Recovered 1 sidebar in place."), "{stdout}");
    assert!(!stdout.contains("not docked"), "{stdout}");
    let sidebars: Vec<_> = after
        .panes
        .iter()
        .filter(|pane| pane.is_sidebar())
        .collect();
    assert_eq!(sidebars.len(), 1, "{after:#?}");
    let sidebar = sidebars[0];
    assert_eq!(sidebar.pane_x, 0, "{sidebar:?}");
    assert_eq!(sidebar.pane_y, 0, "{sidebar:?}");
    assert_eq!(sidebar.pane_rows, 50, "{sidebar:?}");
    assert!((sidebar.pane_columns as i64 - 60).abs() <= 5, "{sidebar:?}");
    // The stub serves a sidebar from one process, so a second one is the
    // anchored split's unplaced pane, which the fallback must have reaped.
    assert_eq!(
        serve_processes_for(fixture.room.name()).expect("scan sidebar serve processes"),
        1,
        "the unplaced anchored sidebar's serve process was reaped",
    );
    let work_after = after
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == work.id)
        .unwrap();
    assert_eq!(work_after.pane_y, work.pane_y, "{work_after:?}");
    assert_eq!(work_after.pane_rows, work.pane_rows, "{work_after:?}");
    assert!(
        after
            .panes
            .iter()
            .any(|pane| !pane.is_plugin && pane.is_floating && pane.id == floating.id),
        "the floating pane must survive: {after:#?}"
    );
    assert!(
        !floating_visible(),
        "the fallback must hide, not close, floating panes"
    );
}

#[test]
fn repair_cli_defers_a_detached_add_and_exits_one() {
    require_zellij!();
    let fixture = RepairRoom::new("repairdefer");
    fixture.birth(r#"tab name="work" { pane cwd=CWD { command "sleep"; args "600"; }; }"#);
    let output = fixture.repair();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Deferred 1 sidebar repair"), "{stdout}");
    assert_eq!(output.status.code(), Some(1), "{stdout}");
    insta::assert_snapshot!(stdout, @r###"
    Presence plugin silent or stale in 1 session; repair ran from Zellij's live pane list. Run `rimz doctor`; reattach or restart the session to reload the plugin.
    Deferred 1 sidebar repair (no attached client); attach and re-run `rimz sidebar repair`.
    "###);
    let snapshot = expect_list_panes(fixture.room.path(), fixture.room.name());
    assert_eq!(
        snapshot.panes.iter().filter(|pane| !pane.is_plugin).count(),
        1,
        "{snapshot:#?}"
    );
}

#[test]
fn repair_cli_reports_a_frozen_roster_without_duplicates_or_orphan_tabs() {
    require_zellij!();
    let fixture = RepairRoom::new("repairfrozen");
    fixture.birth(
        r#"
tab name="ghost" { pane name="rimz-sidebar" cwd=CWD { command STUB; }; }
tab name="orphan" { pane name="rimz-sidebar" cwd=CWD { command STUB; }; }
tab name="kept" split_direction="vertical" {
    pane size="25%" name="rimz-sidebar" cwd=CWD { command STUB; }
    pane cwd=CWD { command "sleep"; args "600"; }
}
tab name="missing" split_direction="horizontal" {
    pane focus=true cwd=CWD { command "sleep"; args "600"; }
    pane cwd=CWD { command "sleep"; args "600"; }
}
"#,
    );
    let mut client = AttachedClient::attach(&fixture.room, 180, 50);
    let initial = expect_list_panes(fixture.room.path(), fixture.room.name());
    for tab in ["kept", "missing"] {
        let work = initial
            .panes
            .iter()
            .find(|pane| pane.tab_name.as_deref() == Some(tab) && !pane.is_sidebar())
            .unwrap();
        client.press_alt_until(
            'l',
            &PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", work.id)),
            "view each work tab at the attached viewport",
        );
    }
    let before = expect_list_panes(fixture.room.path(), fixture.room.name());
    let ghost = before
        .panes
        .iter()
        .find(|pane| pane.tab_name.as_deref() == Some("ghost"))
        .unwrap();
    let kept = before
        .panes
        .iter()
        .find(|pane| pane.tab_name.as_deref() == Some("kept") && pane.is_sidebar())
        .unwrap()
        .id;
    fixture
        .room
        .backend()
        .close_pane(
            fixture.room.name(),
            &PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", ghost.id)),
        )
        .unwrap();
    fixture.claim(Some(kept), &SidebarInstanceId::new());
    // The cache fixture resolves the named workspace under its runtime root.
    publish_room_bin(fixture.room.path(), &fixture.opts);
    let _frozen = stale_topology_keepalive(
        fixture.room.path(),
        &fixture.opts.workspace_id,
        fixture.room.name(),
        before,
    );
    let (output, _) = with_repair_claims(&fixture, || fixture.repair());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("silent or stale"), "{stdout}");
    assert!(
        output.status.success(),
        "{stdout}\n{}\n{:#?}",
        String::from_utf8_lossy(&output.stderr),
        expect_list_panes(fixture.room.path(), fixture.room.name())
    );
    let after = expect_list_panes(fixture.room.path(), fixture.room.name());
    assert_eq!(after.tab_ids().len(), 2, "{after:?}");
    let sidebars: Vec<_> = after
        .panes
        .iter()
        .filter(|pane| pane.is_sidebar())
        .collect();
    assert_eq!(sidebars.len(), 2, "{after:?}");
    assert!(
        sidebars.iter().any(|pane| pane.id == kept),
        "claimed pane was replaced: {after:?}"
    );
    for sidebar in sidebars {
        assert_eq!(sidebar.pane_x, 0, "{sidebar:?}");
        assert_eq!(sidebar.pane_y, 0, "{sidebar:?}");
        assert_eq!(sidebar.pane_rows, 50, "{sidebar:?}");
        assert!((sidebar.pane_columns as i64 - 45).abs() <= 5, "{sidebar:?}");
    }
    let state = rimz::StatePaths::for_project_root_under(
        &fixture.env.project_root,
        &fixture.env.rimz_home(),
    )
    .unwrap();
    let log =
        rimz::diag::DiagSink::under(state.root, state.workspace_id, fixture.room.name(), None)
            .log_path()
            .unwrap();
    let diagnostics = std::fs::read_to_string(log).unwrap();
    assert!(
        diagnostics.contains("presence_roster_stale"),
        "{diagnostics}"
    );
}

#[test]
fn concurrent_attach_reconciles_keep_the_first_mounted_sidebar() {
    require_zellij!();
    let fixture = RepairRoom::new("repairlock");
    fixture.birth(r#"tab name="work" { pane cwd=CWD { command "sleep"; args "600"; }; }"#);
    let _client = AttachedClient::attach(&fixture.room, 180, 50);
    fixture.claim(Some(999_999), &SidebarInstanceId::new());
    fixture.room.backend().version().unwrap();
    let barrier = std::sync::Barrier::new(2);
    let ((), first_pane) = with_repair_claims(&fixture, || {
        std::thread::scope(|scope| {
            let run = || {
                barrier.wait();
                let outcome = rimz::sidebar::launch_sidebar_if_needed(
                    fixture.room.backend(),
                    &fixture.opts.runtime,
                    &fixture.opts,
                    None,
                );
                assert_eq!(outcome, rimz::sidebar::SidebarLaunchOutcome::SkippedFresh);
            };
            let first = scope.spawn(run);
            let second = scope.spawn(run);
            first.join().unwrap();
            second.join().unwrap();
        })
    });
    let after = expect_list_panes(fixture.room.path(), fixture.room.name());
    let sidebars: Vec<_> = after
        .panes
        .iter()
        .filter(|pane| pane.is_sidebar())
        .map(|pane| pane.id)
        .collect();
    assert_eq!(
        sidebars,
        [first_pane.expect("a sidebar mounted")],
        "{after:?}"
    );
}

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
    record_known_workspace_session(&env.rimz_home(), &workspace_id, &project_root, &name);
    assert!(
        rimz::workspace::known_workspaces_under(&rimz::disk::paths::workspaces_dir_under(
            &env.rimz_home()
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
    // The live pane list proves the working pane exists, but no client is
    // attached to mount its missing sidebar.
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
    assert_eq!(
        after_ids,
        before_ids,
        "every work pane survives the add: panes={:?}",
        expect_list_panes(&xdg, &name)
    );
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

#[test]
fn replacement_preserves_work_rectangles_across_a_midline_border() {
    require_zellij!();
    assert_replacement_preserves_work_rectangles(false);
}

#[test]
fn replacement_preserves_work_rectangles_in_a_daemon_shaped_tab() {
    require_zellij!();
    assert_replacement_preserves_work_rectangles(true);
}

fn assert_replacement_preserves_work_rectangles(daemon: bool) {
    let room = LiveZellijSession::new("replace-rectangles");
    let version = room.backend().version().expect("version");
    if version
        .split('.')
        .nth(1)
        .and_then(|part| part.parse::<u32>().ok())
        .is_none_or(|minor| minor < 45)
    {
        crate::common::skip("zellij below 0.45");
        return;
    }
    let cwd = TempDir::new().expect("cwd");
    let (_stub_dir, stub) = sidebar_stub_alive_for(120);
    let layout = write_kdl_layout(cwd.path(), &stub, "replace.kdl", |cwd_kdl, stub_kdl| {
        let work = if daemon {
            r#"pane name="content" focus=true { command "sleep"; args "600"; }
        pane split_direction="horizontal" {
            pane name="host-one" { command "sleep"; args "600"; }
            pane name="host-two" { command "sleep"; args "600"; }
        }"#
        } else {
            r#"pane split_direction="horizontal" {
            pane name="upper-work" focus=true { command "sleep"; args "600"; }
            pane name="lower-work" { command "sleep"; args "600"; }
        }"#
        };
        format!(
            r#"layout {{
    pane split_direction="vertical" {{
        pane size="25%" name="rimz-sidebar" borderless=true cwd={cwd_kdl} {{
            command {stub_kdl}
            close_on_exit true
        }}
        {work}
    }}
}}
"#
        )
    });
    birth_kdl_session(&room, cwd.path(), &layout, "replacement rectangles");
    let mut client = AttachedClient::attach(&room, 240, 60);
    let name = room.name();
    let xdg = room.path();
    wait_for_stable_terminal_state(xdg, name, Duration::from_millis(500));
    let before = expect_list_panes(xdg, name);
    let old = before.sidebar().expect("old sidebar");
    let work = work_pane_geometry(xdg, name);
    let focused = before
        .panes
        .iter()
        .find(|pane| pane.title.as_deref() == Some(if daemon { "content" } else { "upper-work" }))
        .expect("focused work pane");
    client.wait_until_focused(
        &PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", focused.id)),
        "working pane before replacement",
    );
    client.send_line("replacement-client-input");
    let opts = reconcile_opts(
        name,
        "/tmp/rimz-replace-rectangles",
        cwd.path(),
        cwd.path(),
        stub,
        240,
    );
    write_topology_cache_from_list_panes(xdg, &opts.workspace_id, name);
    let _mirror = topology_cache_mirror(xdg, &opts.workspace_id, name);
    let report = room
        .backend()
        .reconcile_sidebars(&opts, &SidebarLiveness::default())
        .expect("replace");
    assert_eq!(
        work_pane_geometry(xdg, name),
        work,
        "replacement must not move any work rectangle"
    );
    assert_eq!(report.recovered, 1, "{report:?}");
    assert_eq!(report.closed, 1, "{report:?}");
    assert_eq!(
        report.failed + report.misdocked + report.deferred,
        0,
        "{report:?}"
    );
    let after = expect_list_panes(xdg, name);
    let sidebars: Vec<_> = after
        .panes
        .iter()
        .filter(|pane| pane.is_sidebar())
        .collect();
    assert_eq!(sidebars.len(), 1, "{after:?}");
    assert_ne!(sidebars[0].id, old.id);
    assert_eq!(
        report.repairs,
        [rimz::mux::SidebarViewRepair {
            view: old.tab_id.to_string(),
            closed: vec![PaneId::from_parts(
                MuxName::Zellij,
                format!("terminal_{}", old.id)
            )],
            added: Some(PaneId::from_parts(
                MuxName::Zellij,
                format!("terminal_{}", sidebars[0].id)
            )),
        }]
    );
    assert!(
        !after
            .panes
            .iter()
            .any(|pane| pane.id == old.id && !pane.is_plugin)
    );
    assert_eq!(sidebars[0].pane_x, 0);
    assert_eq!(sidebars[0].pane_y, old.pane_y);
    assert_eq!(sidebars[0].pane_rows, old.pane_rows);
    assert_eq!(
        sidebars[0].pane_columns,
        u64::from(opts.target.cols(Some(240)).get())
    );
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
    create_kdl_session(session, session.command(), cwd, layout, label);
}

fn create_kdl_session(
    session: &LiveZellijSession,
    mut create: std::process::Command,
    cwd: &Path,
    layout: &Path,
    label: &str,
) {
    let created = create
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
        runtime: rimz::RuntimePaths::for_state_under(
            &rimz::StatePaths::under(
                WorkspaceId::from_project_root(Path::new(workspace_root)),
                stub.parent().expect("stub directory"),
            )
            .expect("fixture state"),
            stub.parent().expect("stub directory"),
        ),
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
