use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rimz::ids::{MuxName, PaneId, WorkspaceId};
use rimz::mux::{LayoutPanes, MuxBackend, PaneCmd, TabOptions, ZellijBackend, zellij};
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, ZellijNamespace};

use super::support::*;

#[test]
fn live_work_boundary_resize_is_audited() {
    require_zellij!();

    const VIEW_COLS: u16 = 213;
    let room = LiveZellijSession::new("boundary-audit");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let sidebar = sidebar_opts(&name, cwd.path(), stub, VIEW_COLS);
    let backend = ZellijBackend::with_runtime_dir(xdg);
    publish_room_bin(xdg, &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open sidebar");
    wait_for_pane_count(xdg, &name, 2);
    let _client = AttachedClient::attach(&room, VIEW_COLS, 60);
    backend
        .open_tab(&TabOptions {
            title: "audit".to_owned(),
            panes: LayoutPanes {
                columns: vec![
                    tiled_column(vec![PaneCmd {
                        argv: vec!["sleep".to_owned(), "600".to_owned()],
                        name: Some("architect".to_owned()),
                    }]),
                    tiled_column(vec![PaneCmd {
                        argv: vec!["sleep".to_owned(), "600".to_owned()],
                        name: Some("work".to_owned()),
                    }]),
                ],
            },
            focus: true,
            dock_sidebar: true,
            sidebar: sidebar.clone(),
        })
        .expect("open audit tab");
    let before = wait_for_named_work_pane_count(xdg, &name, "audit", 2);
    let state = rimz::StatePaths::under(sidebar.workspace_id.clone(), xdg).expect("state paths");
    let runtime =
        rimz::RuntimePaths::under(sidebar.workspace_id.clone(), xdg).expect("runtime paths");
    state.ensure_dirs().expect("state dirs");
    runtime.ensure_dirs().expect("runtime dirs");
    let mut baseline = topology_cache_from_list_panes(xdg, &name);
    let writer = rimz::sidebar::cache::read_pane_topology_cache(&runtime, &name)
        .and_then(|cache| cache.writer);
    baseline.writer = writer.clone();
    rimz::sidebar::presence::ingest_zellij_wake(
        &state,
        &runtime,
        &rimz::sidebar::presence::ZellijWake {
            reason: rimz::sidebar::presence::ZellijWakeReason::Alive,
            session_name: Some(name.clone()),
            pane_id: None,
            active_tab: None,
            focus_generation: None,
            focus_clients: Vec::new(),
            topology: Some(baseline),
            telemetry: None,
        },
    )
    .expect("ingest baseline topology");

    let target = format!("terminal_{}", before[0].id);
    let output = ZellijNamespace::command_at(xdg)
        .args([
            "--session",
            &name,
            "action",
            "resize",
            "decrease",
            "right",
            "--pane-id",
            &target,
        ])
        .bounded_output()
        .expect("resize work boundary");
    assert!(
        output.status.success(),
        "resize work boundary failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    wait_for_named_work_pane_state(xdg, &name, "audit", 2, |after| after != &before);
    let mut incoming = topology_cache_from_list_panes(xdg, &name);
    incoming.writer = writer;
    rimz::sidebar::presence::ingest_zellij_wake(
        &state,
        &runtime,
        &rimz::sidebar::presence::ZellijWake {
            reason: rimz::sidebar::presence::ZellijWakeReason::Alive,
            session_name: Some(name.clone()),
            pane_id: None,
            active_tab: None,
            focus_generation: None,
            focus_clients: Vec::new(),
            topology: Some(incoming),
            telemetry: None,
        },
    )
    .expect("ingest moved topology");

    let diag =
        rimz::diag::DiagSink::under(state.root.clone(), state.workspace_id.clone(), &name, None);
    let records = std::fs::read_to_string(diag.log_path().expect("diagnostic path"))
        .expect("boundary diagnostic");
    let moves = records
        .lines()
        .filter_map(|line| serde_json::from_str::<rimz::diag::record::DiagEnvelope>(line).ok())
        .filter(|record| {
            matches!(
                record.event,
                rimz::diag::record::DiagEvent::WorkPaneBoundaryMoved { .. }
            )
        })
        .count();
    assert_eq!(moves, 1, "expected one boundary audit record: {records}");
}

/// The presence-plugin wasm `cargo xtask build-plugin` produces, honoring
/// `CARGO_TARGET_DIR`. `None` self-skips the live plugin test — CI's
/// build-plugin gate runs before the suite, so the artifact is present there.
pub(in crate::backend::zellij) fn presence_wasm_artifact() -> Option<PathBuf> {
    let target_root = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target"));
    let wasm = target_root.join("wasm32-wasip1/release/rimz-presence-zellij.wasm");
    // Canonical, because the permission grant the test seeds is keyed on the
    // exact path string Zellij sees — a `../..` in it misses the grant.
    wasm.canonicalize().ok().filter(|wasm| wasm.is_file())
}

pub(in crate::backend::zellij) fn seed_presence_permissions(xdg: &Path, wasm: &Path) {
    let cache_dir = xdg.join("zellij");
    std::fs::create_dir_all(&cache_dir).expect("zellij cache dir");
    std::fs::write(
        cache_dir.join("permissions.kdl"),
        format!(
            "\"{}\" {{\n    ReadApplicationState\n    RunCommands\n    Reconfigure\n}}\n",
            wasm.display(),
        ),
    )
    .expect("seed permission grant");
}

fn write_poke_shim(dir: &Path, log: &Path, real_rimz: &Path, focus_exec_log: &Path) -> PathBuf {
    let rimz_shim = dir.join("rimz-poke-shim");
    std::fs::write(
        &rimz_shim,
        format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> {log}\n\
             if [ \"${{1:-}}\" = \"sidebar\" ] && [ \"${{2:-}}\" = \"focus\" ]; then\n\
               {real_rimz} \"$@\" >> {focus_exec_log} 2>&1\n\
               printf 'exit=%s\\n' \"$?\" >> {focus_exec_log}\n\
             elif [ \"${{1:-}}\" = \"sidebar\" ] && [ \"${{2:-}}\" = \"serve\" ]; then\n\
               {real_rimz} \"$@\" >/dev/null 2>> {focus_exec_log}\n\
             else\n\
               {real_rimz} \"$@\" >/dev/null 2>&1\n\
             fi\n",
            log = sh_quote(&log.display().to_string()),
            real_rimz = sh_quote(&real_rimz.display().to_string()),
            focus_exec_log = sh_quote(&focus_exec_log.display().to_string()),
        ),
    )
    .expect("write poke shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&rimz_shim)
            .expect("metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&rimz_shim, perms).expect("chmod");
    }
    rimz_shim
}

fn sh_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Poll `log` until it holds at least `at_least` lines (or panic at the
/// deadline). The presence plugin pokes through Zellij's `run_command`, so
/// arrival is asynchronous to the load verb returning.
fn wait_for_poke_lines(log: &Path, at_least: usize) -> Vec<String> {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        let lines = poke_lines(log);
        if lines.len() >= at_least {
            return lines;
        }
        if Instant::now() > deadline {
            panic!(
                "expected {at_least}+ poke lines in {}; got {lines:?}",
                log.display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn poke_lines(log: &Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .map(|s| s.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn wait_for_focus_exec_log(log: &Path) -> String {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        let contents = std::fs::read_to_string(log).unwrap_or_default();
        if contents.lines().any(|line| line.starts_with("exit=")) {
            return contents;
        }
        if Instant::now() > deadline {
            panic!(
                "expected focus exec result in {}; got {contents:?}",
                log.display()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn logged_arg<'a>(line: &'a str, flag: &str) -> Option<&'a str> {
    let mut args = line.split_whitespace();
    while let Some(arg) = args.next() {
        if arg == flag {
            return args.next();
        }
    }
    None
}

fn focus_action_count(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .map(|contents| {
            contents
                .lines()
                .filter(|line| line.contains("action focus-pane-id"))
                .count()
        })
        .unwrap_or_default()
}

fn presence_plugin_panes(xdg: &Path, session: &str) -> Result<Vec<ListedPane>, String> {
    list_panes(xdg, session).map(|snapshot| {
        snapshot
            .panes
            .into_iter()
            .filter(|pane| {
                pane.is_plugin
                    && pane
                        .title
                        .as_deref()
                        .is_some_and(|title| title.contains("rimz-presence-zellij"))
            })
            .collect()
    })
}

fn wait_for_focus_action(
    log: &Path,
    renderer_log: &Path,
    pane: &PaneId,
    prior_count: usize,
    phase: &str,
) -> String {
    let raw = pane.raw();
    let numeric = raw.strip_prefix("terminal_").unwrap_or(raw);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let contents = std::fs::read_to_string(log).unwrap_or_default();
        if contents
            .lines()
            .filter(|line| line.contains("action focus-pane-id"))
            .skip(prior_count)
            .any(|line| {
                line.contains("action focus-pane-id")
                    && line
                        .split_whitespace()
                        .any(|arg| arg == raw || arg == numeric)
            })
        {
            return contents;
        }
        if Instant::now() >= deadline {
            panic!(
                "timed out waiting for {phase} focus action on {pane} (ignoring {prior_count} earlier action(s)); trace: {contents}; renderer: {}",
                std::fs::read_to_string(renderer_log).unwrap_or_default(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn accepted_focus_repairs(xdg: &Path, target_pane: &PaneId) -> usize {
    rimz::diag::focus_repair::recent(xdg)
        .iter()
        .filter(|record| {
            record.target == *target_pane
                && record.outcome
                    == rimz::diag::focus_repair::FocusRepairOutcome::AcceptedUnconfirmed
        })
        .count()
}

fn wait_for_accepted_focus_repair(
    xdg: &Path,
    target_pane: &PaneId,
    prior_count: usize,
) -> Vec<rimz::diag::focus_repair::FocusRepairRecord> {
    poll_until(
        Duration::from_secs(10),
        || Ok(rimz::diag::focus_repair::recent(xdg)),
        |records| {
            records
                .iter()
                .filter(|record| {
                    record.target == *target_pane
                        && record.outcome
                            == rimz::diag::focus_repair::FocusRepairOutcome::AcceptedUnconfirmed
                })
                .count()
                > prior_count
        },
        "accepted-unconfirmed focus repair diagnostic",
    )
}

fn wait_for_reload_baseline(log: &Path, prior_lines: usize, pane: &PaneId) -> Vec<String> {
    let raw = pane.raw();
    let numeric = raw.strip_prefix("terminal_").unwrap_or(raw);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let lines = poke_lines(log);
        let reloaded = lines[prior_lines..]
            .iter()
            .any(|line| line.contains("sidebar wake --reason alive"));
        let observed = lines[prior_lines..].iter().any(|line| {
            line.contains("\"clients\"")
                && line.contains(&format!("\"kind\":\"terminal\",\"id\":{numeric}"))
        });
        if reloaded && observed {
            return lines;
        }
        assert!(
            Instant::now() < deadline,
            "converged presence plugin established no client baseline; pokes: {lines:?}",
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_switch_settled(log: &Path, prior_lines: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let lines = poke_lines(log);
        if lines[prior_lines..]
            .iter()
            .any(|line| line.contains("sidebar wake --reason switch-settled"))
        {
            return lines;
        }
        assert!(
            Instant::now() < deadline,
            "presence plugin emitted no switch-settled wake; pokes: {lines:?}",
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// End to end against a live Zellij: the pipe verb loads the presence plugin
/// headlessly, the seeded grant (the test's scoped cache, never the user's)
/// covers it, the load-time configuration reaches the plugin, and its first
/// poke runs the pinned `rimz sidebar wake` argv. Then the converge verb
/// (`rimz reload`'s upgrade path) addresses that same identity through the
/// pipe and requests a fresh topology dump without duplicating the instance.
#[test]
fn presence_plugin_loads_pokes_and_converges_on_a_live_session() {
    require_zellij!();
    let Some(wasm) = presence_wasm_artifact() else {
        eprintln!("presence wasm not built (run `cargo xtask build-plugin`); skipping test");
        return;
    };
    match zellij::capabilities() {
        Ok(caps)
            if caps
                .parsed_version
                .is_some_and(|v| v >= zellij::MIN_ZELLIJ_VERSION) => {}
        _ => {
            eprintln!("zellij below the presence-plugin floor; skipping test");
            return;
        }
    }

    // Seed the grant before the server is born so its permission cache read —
    // whenever it happens — sees it. Grants key on the wasm's absolute path.
    let namespace = ZellijNamespace::new();
    seed_presence_permissions(namespace.path(), &wasm);

    // A `rimz` stand-in that logs its argv: the poke's whole host surface.
    let poke_log = namespace.path().join("poke.log");
    let focus_exec_log = namespace.path().join("focus-exec.log");
    let real_rimz = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    let rimz_shim = write_poke_shim(namespace.path(), &poke_log, &real_rimz, &focus_exec_log);

    // Born on the pre-seeded dir with a PTY client attached: application
    // state flows only while a client is connected, and the cached grant is
    // proven to the plugin by exactly that flow (Zellij sends no explicit
    // permission result for a cached grant).
    let name = unique_session_name("presence");
    let room = LiveZellijSession::from_namespace(namespace, name.clone());
    let _client = AttachedClient::create_and_attach(&room, 80, 24);

    let backend = ZellijBackend::with_runtime_dir(room.path());
    let workspace_id = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("fixed id");
    let mut opts = rimz::mux::PresencePluginOptions {
        session_name: name.clone(),
        workspace_id: workspace_id.clone(),
        wasm,
        rimz_bin: rimz_shim,
        converge: false,
        focus_key: None,
        zoom_key: None,
        focus_follows_mouse: false,
        mouse_click_through: true,
    };
    backend
        .ensure_presence_plugin(&opts)
        .expect("pipe load against a live session");

    let lines = wait_for_poke_lines(&poke_log, 1);
    assert!(
        lines[0]
            .starts_with("sidebar wake --reason alive --workspace-id ws_0123456789abcdef01234567"),
        "the first poke is the granted plugin's immediate keepalive, with the \
         load-time configuration threaded through: {:?}",
        lines[0],
    );
    assert_eq!(
        logged_arg(&lines[0], "--session-name"),
        Some(name.as_str()),
        "the first poke carries the session name: {:?}",
        lines[0],
    );
    let telemetry: serde_json::Value = serde_json::from_str(
        logged_arg(&lines[0], "--plugin-telemetry").expect("telemetry payload"),
    )
    .expect("telemetry JSON parses");
    assert!(
        telemetry["mem_pages"]
            .as_u64()
            .is_some_and(|pages| pages > 0),
        "the first poke carries WASM page telemetry: {:?}",
        lines[0],
    );
    assert!(telemetry["uptime_ms"].as_u64().is_some());
    assert_eq!(
        telemetry["commands_completed"], 0,
        "the first poke has not yet completed a run_command reply: {:?}",
        lines[0],
    );
    assert!(
        telemetry["zellij_version"]
            .as_str()
            .is_some_and(|version| !version.is_empty()),
        "the first poke carries the Zellij version: {:?}",
        lines[0],
    );
    let runtime = rimz::store::RuntimePaths::under(workspace_id, room.path())
        .expect("presence runtime paths");
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let writer = loop {
        if let Some(writer) = rimz::sidebar::cache::read_pane_topology_cache(&runtime, &name)
            .and_then(|cache| cache.writer)
        {
            break writer;
        }
        assert!(
            Instant::now() <= deadline,
            "presence plugin never echoed its writer identity",
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        writer.build.as_deref(),
        Some(rimz::mux::zellij::presence_plugin_build())
    );
    assert!(
        writer
            .config
            .as_deref()
            .is_some_and(|hash| !hash.is_empty())
    );

    let plugins_before =
        presence_plugin_panes(room.path(), &name).expect("presence plugin roster before converge");
    assert_eq!(plugins_before.len(), 1);

    // Production reaches this path only for an identity change. The same-
    // identity case proves the pipe stays idempotent and the explicit dump,
    // rather than an in-place reload, republishes topology.
    let before = lines.len();
    opts.converge = true;
    backend
        .ensure_presence_plugin(&opts)
        .expect("converge against a live session");
    let lines = wait_for_poke_lines(&poke_log, before + 1);
    assert!(
        lines[before..]
            .iter()
            .any(|line| line.contains("--reason alive")),
        "a converged plugin republishes topology; got {lines:?}",
    );
    let plugins_after =
        presence_plugin_panes(room.path(), &name).expect("presence plugin roster after converge");
    assert_eq!(plugins_after.len(), 1);
    let current_writer = rimz::sidebar::cache::read_pane_topology_cache(&runtime, &name)
        .and_then(|cache| cache.writer)
        .expect("writer after same-identity converge");
    assert_eq!(current_writer, writer);
}

#[test]
fn presence_identity_transition_keeps_global_background_updates() {
    require_zellij!();
    let Some(wasm) = presence_wasm_artifact() else {
        eprintln!("presence wasm not built (run `cargo xtask build-plugin`); skipping test");
        return;
    };
    match zellij::capabilities() {
        Ok(caps)
            if caps
                .parsed_version
                .is_some_and(|version| version >= zellij::MIN_ZELLIJ_VERSION) => {}
        _ => {
            eprintln!("zellij below the presence-plugin floor; skipping test");
            return;
        }
    }

    let room = LiveZellijSession::new("presence-upgrade");
    let xdg = room.path();
    seed_presence_permissions(xdg, &wasm);
    let cwd = TempDir::new().expect("session cwd tempdir");
    let name = room.name().to_owned();
    room.create_plain_background(cwd.path(), "600");
    let mut client = AttachedClient::attach(&room, 160, 45);

    let backend = ZellijBackend::with_runtime_dir(xdg);
    let workspace_id = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("fixed id");
    let mut opts = rimz::mux::PresencePluginOptions {
        session_name: name.clone(),
        workspace_id: workspace_id.clone(),
        wasm,
        rimz_bin: crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz")),
        converge: false,
        focus_key: None,
        zoom_key: None,
        focus_follows_mouse: false,
        mouse_click_through: true,
    };
    backend
        .ensure_presence_plugin(&opts)
        .expect("load initial presence identity");
    let runtime =
        rimz::store::RuntimePaths::under(workspace_id, xdg).expect("presence runtime paths");
    let initial_cache = poll_until(
        SPAWN_TIMEOUT,
        || {
            Ok(rimz::sidebar::cache::read_pane_topology_cache(
                &runtime, &name,
            ))
        },
        |cache| {
            cache
                .as_ref()
                .and_then(|cache| cache.writer.as_ref())
                .is_some()
        },
        "initial presence writer",
    )
    .expect("initial topology cache");
    let initial_writer = initial_cache.writer.expect("initial writer");

    open_new_tab(xdg, &name);
    let snapshot = expect_list_panes(xdg, &name);
    let mut tab_work = snapshot
        .panes
        .iter()
        .filter(|pane| pane.is_live_terminal())
        .map(|pane| (pane.tab_position.unwrap_or(u64::MAX), pane.tab_id, pane.id))
        .collect::<Vec<_>>();
    tab_work.sort_unstable();
    tab_work.dedup_by_key(|(position, _, _)| *position);
    assert_eq!(tab_work.len(), 2, "expected one work pane in each tab");
    let (_, _, first_work) = tab_work[0];
    let (_, second_tab_id, second_work) = tab_work[1];
    let first_work = PaneId::from_parts(MuxName::Zellij, format!("terminal_{first_work}"));
    let second_work_pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{second_work}"));
    client.go_to_tab_until(1, &first_work, "first tab");

    // A configuration change is the same Zellij identity transition as a wasm
    // upgrade. Converge while tab one owns the attached client's focus.
    opts.focus_key = Some("Alt+p".to_owned());
    opts.converge = true;
    backend
        .ensure_presence_plugin(&opts)
        .expect("converge changed presence identity");
    let changed_cache = poll_until(
        SPAWN_TIMEOUT,
        || {
            Ok(rimz::sidebar::cache::read_pane_topology_cache(
                &runtime, &name,
            ))
        },
        |cache| {
            cache
                .as_ref()
                .and_then(|cache| cache.writer.as_ref())
                .is_some_and(|writer| writer.config != initial_writer.config)
        },
        "changed presence writer",
    )
    .expect("changed topology cache");
    let changed_writer = changed_cache.writer.expect("changed writer");
    assert_ne!(changed_writer.config, initial_writer.config);
    assert!(changed_writer.generation() > initial_writer.generation());

    let plugins = poll_until(
        SPAWN_TIMEOUT,
        || presence_plugin_panes(xdg, &name),
        |plugins| plugins.len() == 1,
        "one presence plugin after identity convergence",
    );
    assert!(
        plugins[0].is_suppressed,
        "the background writer must not occupy a visible tiled pane: {plugins:?}",
    );
    assert_eq!(u64::from(changed_writer.plugin_id), plugins[0].id);

    // Move away from the tab active during convergence, then create and focus
    // a pane. A tab-scoped writer starves here; a background writer publishes
    // both the new topology and the attached client's selection.
    client.go_to_tab_until(2, &second_work_pane, "second tab");
    let before_ids = expect_list_panes(xdg, &name)
        .panes
        .into_iter()
        .filter(|pane| pane.is_live_terminal())
        .map(|pane| pane.id)
        .collect::<BTreeSet<_>>();
    spawn_sleep_pane(xdg, &name, cwd.path());
    let fresh = expect_list_panes(xdg, &name)
        .panes
        .into_iter()
        .find(|pane| {
            pane.is_live_terminal()
                && pane.tab_id == second_tab_id
                && !before_ids.contains(&pane.id)
        })
        .expect("fresh pane in the second tab");
    let updated = poll_until(
        SPAWN_TIMEOUT,
        || {
            Ok(rimz::sidebar::cache::read_pane_topology_cache(
                &runtime, &name,
            ))
        },
        |cache| {
            cache.as_ref().is_some_and(|cache| {
                cache.writer.as_ref() == Some(&changed_writer)
                    && cache
                        .panes
                        .iter()
                        .any(|pane| !pane.is_plugin && pane.id == fresh.id)
                    && cache.clients.as_ref().is_some_and(|clients| {
                        clients
                            .views
                            .iter()
                            .any(|view| view.pane_id.terminal_id() == Some(fresh.id))
                    })
            })
        },
        "background topology and focus update from the second tab",
    )
    .expect("updated topology cache");
    assert_eq!(updated.writer.as_ref(), Some(&changed_writer));
}

#[test]
fn tab_switch_repairs_sidebar_focus_from_attached_client_views() {
    require_zellij!();
    let real_rimz = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    if !real_rimz.exists() {
        eprintln!("rimz binary not built; skipping tab-switch repair test");
        return;
    }
    match zellij::capabilities() {
        Ok(caps)
            if caps
                .parsed_version
                .is_some_and(|version| version >= zellij::MIN_ZELLIJ_VERSION) => {}
        _ => {
            eprintln!("zellij below the presence-plugin floor; skipping test");
            return;
        }
    }

    let room = LiveZellijSession::new("switch-repair");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("session cwd tempdir");
    let poke_log = xdg.join("poke.log");
    let focus_exec_log = xdg.join("focus-exec.log");
    let rimz_shim = write_poke_shim(xdg, &poke_log, &real_rimz, &focus_exec_log);
    let trace = TempDir::new().expect("zellij trace tempdir");
    let trace_log = trace.path().join("zellij.log");
    let zellij_shim = trace.path().join("zellij");
    let real_zellij = which::which("zellij").expect("zellij path");
    std::fs::write(
        &zellij_shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\nexec {} \"$@\"\n",
            sh_quote(&trace_log.display().to_string()),
            sh_quote(&real_zellij.display().to_string()),
        ),
    )
    .expect("write zellij trace shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&zellij_shim)
            .expect("zellij trace shim metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&zellij_shim, permissions).expect("chmod zellij trace shim");
    }

    let wasm = zellij::ensure_presence_plugin_artifact().expect("materialized presence plugin");
    let mut sidebar = sidebar_opts(&name, cwd.path(), rimz_shim.clone(), 160);
    sidebar.extra_env.insert(
        "RIMZ_ZELLIJ_BIN".to_owned(),
        zellij_shim.display().to_string(),
    );
    sidebar.extra_env.insert(
        "RIMZ_PRESENCE_PLUGIN".to_owned(),
        wasm.display().to_string(),
    );
    sidebar
        .extra_env
        .insert("RUST_LOG".to_owned(), "rimz=debug".to_owned());
    let backend = ZellijBackend::with_runtime_dir(xdg);
    publish_room_bin(xdg, &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open sidebar");
    wait_for_pane_count(xdg, &name, 2);

    let mut client = AttachedClient::attach(&room, 160, 45);
    let birth_sidebar = raw_sidebar_pane(xdg, &name);
    let birth_work = expect_list_panes(xdg, &name)
        .panes
        .iter()
        .find(|pane| {
            pane.is_live_terminal()
                && pane.tab_id == birth_sidebar.tab_id
                && pane.id != birth_sidebar.id
        })
        .map(|pane| PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", pane.id)))
        .expect("birth work pane");
    client.press_alt_until('l', &birth_work, "birth work pane");

    let target_tab = "switch target";
    let input_log = cwd.path().join("routed-input.log");
    backend
        .open_tab(&TabOptions {
            title: target_tab.to_owned(),
            panes: LayoutPanes {
                columns: vec![tiled_column(vec![PaneCmd {
                    argv: vec![
                        "sh".to_owned(),
                        "-c".to_owned(),
                        format!(
                            "while IFS= read -r line; do printf '%s\\n' \"$line\" >> {}; done",
                            sh_quote(&input_log.display().to_string()),
                        ),
                    ],
                    name: None,
                }])],
            },
            focus: true,
            dock_sidebar: true,
            sidebar: sidebar.clone(),
        })
        .expect("open switch target tab");
    let target_work = wait_for_named_work_pane_count(xdg, &name, target_tab, 1)[0];
    let target_sidebar =
        wait_for_named_sidebar_pane(xdg, &name, target_tab).expect("target tab sidebar");
    let target_work = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", target_work.id));
    let target_sidebar =
        PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", target_sidebar.id));
    client.go_to_tab_until(2, &target_work, "target work pane");

    client.press_alt_until('h', &target_sidebar, "stranded target sidebar");

    client.go_to_tab_until(1, &birth_work, "birth work pane before switch repair");
    let pokes_before = poke_lines(&poke_log).len();
    let actions_before = focus_action_count(&trace_log);
    let repairs_before = accepted_focus_repairs(xdg, &target_work);
    client.go_to_tab(2);
    let settled = wait_for_switch_settled(&poke_log, pokes_before);
    assert!(
        settled[pokes_before..].iter().any(|line| {
            line.contains("--active-tab")
                && line.contains("--focus-generation")
                && line.contains("--focus-clients")
        }),
        "settled wake lacks tab/generation/client evidence: {settled:?}",
    );
    wait_for_focus_action(
        &trace_log,
        &focus_exec_log,
        &target_work,
        actions_before,
        "first switch-settled repair",
    );
    let repairs = wait_for_accepted_focus_repair(xdg, &target_work, repairs_before);

    client.assert_input_reaches(&target_work, "repaired target work pane");

    assert!(repairs.iter().any(|record| {
        record.target == target_work
            && record.outcome == rimz::diag::focus_repair::FocusRepairOutcome::AcceptedUnconfirmed
    }));

    client.press_alt_until(
        'h',
        &target_sidebar,
        "stranded target sidebar before reload",
    );
    client.go_to_tab_until(1, &birth_work, "birth work pane before presence reload");
    let pokes_before_reload = poke_lines(&poke_log).len();
    let room_bin = rimz::StatePaths::under(sidebar.workspace_id.clone(), xdg)
        .expect("room state paths")
        .room_bin;
    backend
        .ensure_presence_plugin(&rimz::mux::PresencePluginOptions {
            session_name: name.clone(),
            workspace_id: sidebar.workspace_id.clone(),
            wasm,
            rimz_bin: room_bin,
            converge: true,
            focus_key: Some("Alt+p".to_owned()),
            zoom_key: Some("Alt+z".to_owned()),
            focus_follows_mouse: false,
            mouse_click_through: true,
        })
        .expect("converge presence plugin");
    wait_for_reload_baseline(&poke_log, pokes_before_reload, &birth_work);
    let pokes_before = poke_lines(&poke_log).len();
    let actions_before = focus_action_count(&trace_log);
    let repairs_before = accepted_focus_repairs(xdg, &target_work);
    client.go_to_tab(2);
    wait_for_switch_settled(&poke_log, pokes_before);
    wait_for_focus_action(
        &trace_log,
        &focus_exec_log,
        &target_work,
        actions_before,
        "post-reload repair",
    );
    wait_for_accepted_focus_repair(xdg, &target_work, repairs_before);
    client.assert_input_reaches(&target_work, "repaired target after plugin reload");

    client.press_alt_until(
        'h',
        &target_sidebar,
        "stranded target sidebar before detach",
    );
    client.go_to_tab_until(1, &birth_work, "birth work pane before detach");
    let actions_before = focus_action_count(&trace_log);
    client.go_to_tab(2);
    drop(client);
    let detached = wait_for_human_client_count(&backend, &name, 0);
    assert!(detached.viewed_panes.is_empty());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(
        focus_action_count(&trace_log),
        actions_before,
        "a settled switch observation acted after the client detached; trace: {}",
        std::fs::read_to_string(&trace_log).unwrap_or_default(),
    );
}

#[test]
fn presence_plugin_keepalive_survives_deleted_launch_cwd() {
    require_zellij!();
    let Some(wasm) = presence_wasm_artifact() else {
        eprintln!("presence wasm not built (run `cargo xtask build-plugin`); skipping test");
        return;
    };
    match zellij::capabilities() {
        Ok(caps)
            if caps
                .parsed_version
                .is_some_and(|v| v >= zellij::MIN_ZELLIJ_VERSION) => {}
        _ => {
            eprintln!("zellij below the presence-plugin floor; skipping test");
            return;
        }
    }

    let namespace = ZellijNamespace::new();
    seed_presence_permissions(namespace.path(), &wasm);
    let real_rimz = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    let name = unique_session_name("presence-cwd");
    let room = LiveZellijSession::from_namespace(namespace, name.clone());
    let _client = AttachedClient::create_and_attach(&room, 80, 24);
    let launch_cwd = TempDir::new().expect("plugin launch cwd");
    let workspace_id = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("fixed id");
    let runtime = rimz::store::RuntimePaths::under(workspace_id, room.path())
        .expect("presence runtime paths");
    let stamp_path = rimz::sidebar::cache::presence_stamp_path(&runtime);
    let plugin_url = format!("file:{}", wasm.display());
    let configuration = format!(
        "workspace_id=ws_0123456789abcdef01234567,session_name={name},rimz_bin={},focus_follows_mouse=false,mouse_click_through=true",
        real_rimz.display(),
    );

    let loaded = room
        .command()
        .current_dir(launch_cwd.path())
        .args([
            "--session",
            &name,
            "pipe",
            "--plugin",
            &plugin_url,
            "--plugin-configuration",
            &configuration,
            "--name",
            "rimz_presence_boot",
            "--",
            "load",
        ])
        .bounded_status()
        .expect("load presence plugin from disposable cwd");
    assert!(loaded.success(), "presence plugin load command failed");
    let mut last_written_at_ms = None;
    let mut stable_observations = 0;
    let before = poll_until(
        SPAWN_TIMEOUT,
        || {
            let bytes = std::fs::read(&stamp_path).map_err(|err| err.to_string())?;
            serde_json::from_slice::<rimz::sidebar::cache::PresenceStamp>(&bytes)
                .map_err(|err| err.to_string())
        },
        |stamp| {
            if last_written_at_ms == Some(stamp.written_at_ms) {
                stable_observations += 1;
            } else {
                last_written_at_ms = Some(stamp.written_at_ms);
                stable_observations = 1;
            }
            stable_observations == 10
        },
        "settled initial presence stamp",
    )
    .written_at_ms;

    launch_cwd.close().expect("delete plugin launch cwd");
    let after = poll_until(
        Duration::from_secs(150),
        || {
            let bytes = std::fs::read(&stamp_path).map_err(|err| err.to_string())?;
            serde_json::from_slice::<rimz::sidebar::cache::PresenceStamp>(&bytes)
                .map_err(|err| err.to_string())
        },
        |stamp| stamp.written_at_ms > before,
        "presence keepalive after deleting the plugin launch cwd",
    );
    assert!(
        after.written_at_ms > before,
        "presence keepalive stopped after its launch cwd was deleted: before={before}, after={}",
        after.written_at_ms,
    );
}

#[test]
fn focus_key_press_from_different_cwd_pipes_sidebar_focus_through_the_plugin() {
    require_zellij!();
    let Some(wasm) = presence_wasm_artifact() else {
        eprintln!("presence wasm not built (run `cargo xtask build-plugin`); skipping test");
        return;
    };
    match zellij::capabilities() {
        Ok(caps)
            if caps
                .parsed_version
                .is_some_and(|v| v >= zellij::MIN_ZELLIJ_VERSION) => {}
        _ => {
            eprintln!("zellij below the presence-plugin floor; skipping test");
            return;
        }
    }

    let namespace = ZellijNamespace::new();
    seed_presence_permissions(namespace.path(), &wasm);
    let poke_log = namespace.path().join("poke.log");
    let focus_exec_log = namespace.path().join("focus-exec.log");
    let real_rimz = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    let rimz_shim = write_poke_shim(namespace.path(), &poke_log, &real_rimz, &focus_exec_log);
    let name = unique_session_name("focuskey");
    let room = LiveZellijSession::from_namespace(namespace, name.clone());
    let mut client = AttachedClient::create_and_attach(&room, 80, 24);

    let backend = ZellijBackend::with_runtime_dir(room.path());
    backend
        .ensure_presence_plugin(&rimz::mux::PresencePluginOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::parse("ws_0123456789abcdef01234567").expect("fixed id"),
            wasm,
            rimz_bin: rimz_shim,
            converge: false,
            focus_key: Some("Alt+p".to_owned()),
            zoom_key: None,
            focus_follows_mouse: false,
            mouse_click_through: true,
        })
        .expect("pipe load against a live session");

    wait_for_poke_lines(&poke_log, 1);

    // Regression shape: the plugin loads from the test process cwd, then the
    // user presses the key from a pane with a different cwd. Url-targeted
    // keybinds miss that running instance; id-targeted keybinds reach it.
    let before: BTreeSet<u64> = work_pane_geometry(room.path(), &name)
        .into_iter()
        .map(|pane| pane.id)
        .collect();
    let pane_cwd = TempDir::new().expect("different cwd tempdir");
    spawn_sleep_pane(room.path(), &name, pane_cwd.path());
    let work_pane = work_pane_geometry(room.path(), &name)
        .into_iter()
        .find(|pane| !before.contains(&pane.id))
        .unwrap_or_else(|| panic!("new different-cwd pane not found; before ids were {before:?}"));
    let work_pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", work_pane.id));
    client.press_alt_until('l', &work_pane, "different-cwd focus-key source pane");

    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let focus_line = loop {
        client.press_alt('p');
        std::thread::sleep(Duration::from_millis(150));

        let lines = poke_lines(&poke_log);
        if let Some(line) = lines
            .into_iter()
            .find(|line| line.contains("sidebar focus"))
        {
            break line;
        }
        if Instant::now() > deadline {
            panic!(
                "Alt+p never piped sidebar focus through the presence plugin; log: {:?}",
                poke_lines(&poke_log)
            );
        }
    };

    assert!(
        focus_line.contains("sidebar focus --toggle"),
        "focus pipe should toggle the sidebar; got {focus_line:?}",
    );
    assert!(
        focus_line.contains(&format!("--session-name {name}")),
        "focus pipe should target the pressing session; got {focus_line:?}",
    );
    assert!(
        focus_line.contains("--mux zellij"),
        "focus pipe should force the Zellij backend; got {focus_line:?}",
    );
    assert!(
        !focus_line.contains("--workspace-id"),
        "focus pipe should not pass a flag sidebar focus rejects; got {focus_line:?}",
    );

    let focus_exec_log = wait_for_focus_exec_log(&focus_exec_log);
    assert!(
        !focus_exec_log.contains("unexpected argument"),
        "real rimz clap rejected the plugin focus argv: {focus_exec_log}",
    );
}
