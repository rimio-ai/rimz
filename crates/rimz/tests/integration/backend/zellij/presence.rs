use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rimz::ids::{MuxName, PaneId, WorkspaceId};
use rimz::mux::{LayoutPanes, MuxBackend, PaneCmd, TabOptions, ZellijBackend, zellij};
use tempfile::TempDir;

use super::support::*;

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
            "\"{}\" {{\n    ReadApplicationState\n    RunCommands\n    Reconfigure\n    StartWebServer\n}}\n",
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

fn wait_for_web_clients_allowed(cache_root: &Path, name: &str) {
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        if rimz::mux::recovery::zellij_session_web_clients_allowed_in(cache_root, name)
            == Some(true)
        {
            return;
        }
        if Instant::now() > deadline {
            panic!(
                "session metadata never reported web_clients_allowed true for {name}; got {:?}",
                rimz::mux::recovery::zellij_session_web_clients_allowed_in(cache_root, name),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
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

fn wait_for_focus_action(
    log: &Path,
    renderer_log: &Path,
    pane: &PaneId,
    prior_count: usize,
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
                "timed out waiting for renderer focus action on {pane}; trace: {contents}; renderer: {}",
                std::fs::read_to_string(renderer_log).unwrap_or_default(),
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn accepted_focus_repairs(xdg: &Path, target_pane: &PaneId) -> usize {
    rimz::harness::assist_log::recent(xdg, None)
        .iter()
        .filter(|record| {
            matches!(
                &record.assist,
                rimz::harness::assist_log::Assist::FocusRepair {
                    target,
                    outcome: rimz::harness::assist_log::FocusRepairOutcome::AcceptedUnconfirmed,
                    ..
                } if target == target_pane
            )
        })
        .count()
}

fn wait_for_accepted_focus_repair(
    xdg: &Path,
    target_pane: &PaneId,
    prior_count: usize,
) -> Vec<rimz::harness::assist_log::AssistRecord> {
    poll_until(
        Duration::from_secs(10),
        || Ok(rimz::harness::assist_log::recent(xdg, None)),
        |records| {
            records
                .iter()
                .filter(|record| {
                    matches!(
                        &record.assist,
                        rimz::harness::assist_log::Assist::FocusRepair {
                            target,
                            outcome: rimz::harness::assist_log::FocusRepairOutcome::AcceptedUnconfirmed,
                            ..
                        } if target == target_pane
                    )
                })
                .count()
                > prior_count
        },
        "accepted-unconfirmed focus repair assist",
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
            "reloaded presence plugin established no client baseline; pokes: {lines:?}",
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
/// (`rimz reload`'s upgrade path) reloads it in place — the reset state pokes
/// a fresh `alive` — proving the two verbs address one instance.
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
    let xdg = scoped_runtime_dir();
    seed_presence_permissions(xdg.path(), &wasm);

    // A `rimz` stand-in that logs its argv: the poke's whole host surface.
    let poke_log = xdg.path().join("poke.log");
    let focus_exec_log = xdg.path().join("focus-exec.log");
    let real_rimz = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    let rimz_shim = write_poke_shim(xdg.path(), &poke_log, &real_rimz, &focus_exec_log);

    // Born on the pre-seeded dir with a PTY client attached: application
    // state flows only while a client is connected, and the cached grant is
    // proven to the plugin by exactly that flow (Zellij sends no explicit
    // permission result for a cached grant).
    let name = unique_session_name("presence");
    let session = ZellijSession::attach_pty(xdg, name.clone(), true);

    let backend = ZellijBackend::with_runtime_dir(session.xdg.path());
    let workspace_id = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("fixed id");
    let mut opts = rimz::mux::PresencePluginOptions {
        session_name: name.clone(),
        workspace_id: workspace_id.clone(),
        wasm,
        rimz_bin: rimz_shim,
        converge: false,
        seed_permissions: false,
        focus_key: None,
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
    assert!(
        logged_arg(&lines[0], "--plugin-mem-pages")
            .expect("telemetry carries WASM pages")
            .parse::<u64>()
            .expect("pages parse")
            > 0,
        "the first poke carries WASM page telemetry: {:?}",
        lines[0],
    );
    logged_arg(&lines[0], "--plugin-uptime-ms")
        .expect("telemetry carries uptime")
        .parse::<u64>()
        .expect("uptime parses");
    assert_eq!(
        logged_arg(&lines[0], "--plugin-commands"),
        Some("0"),
        "the first poke has not yet completed a run_command reply: {:?}",
        lines[0],
    );
    assert!(
        logged_arg(&lines[0], "--plugin-zellij-version").is_some_and(|version| !version.is_empty()),
        "the first poke carries the Zellij version: {:?}",
        lines[0],
    );
    let runtime = rimz::store::RuntimePaths::under(workspace_id, session.xdg.path())
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

    // Converge — `rimz reload`'s upgrade verb — reloads the instance in
    // place: its reset state pokes a fresh `alive` on the next application
    // state. One instance throughout: were a second one launched, its
    // separate keepalive cadence would double the poke stream.
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
        "a converged (reloaded-in-place) plugin re-pokes alive; got {lines:?}",
    );
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

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("switch-repair");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("session cwd tempdir");
    let poke_log = xdg.path().join("poke.log");
    let focus_exec_log = xdg.path().join("focus-exec.log");
    let rimz_shim = write_poke_shim(xdg.path(), &poke_log, &real_rimz, &focus_exec_log);
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
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    publish_room_bin(xdg.path(), &sidebar);
    backend.open_sidebar(&sidebar, None).expect("open sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    let mut client = AttachedClient::attach(xdg.path(), &name, 160, 45);
    wait_for_attached_client(xdg.path(), &name);
    let birth_sidebar = raw_sidebar_pane(xdg.path(), &name);
    let birth_work = expect_list_panes(xdg.path(), &name)
        .panes
        .iter()
        .find(|pane| {
            pane.is_live_terminal()
                && pane.tab_id == birth_sidebar.tab_id
                && pane.id != birth_sidebar.id
        })
        .map(|pane| PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", pane.id)))
        .expect("birth work pane");
    if !client_viewed_panes(&backend, &name)
        .expect("birth client view")
        .contains(&birth_work)
    {
        client.press_alt('l');
        wait_for_focused_client_pane(&backend, &name, &birth_work);
    }

    let target_tab = "switch target";
    let input_log = cwd.path().join("routed-input.log");
    backend
        .open_tab(&TabOptions {
            session_name: name.clone(),
            title: target_tab.to_owned(),
            cwd: cwd.path().to_path_buf(),
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
                }])],
            },
            focus: true,
            dock_sidebar: true,
            sidebar: sidebar.clone(),
        })
        .expect("open switch target tab");
    let target_work = wait_for_named_work_pane_count(xdg.path(), &name, target_tab, 1)[0];
    let target_sidebar =
        wait_for_named_sidebar_pane(xdg.path(), &name, target_tab).expect("target tab sidebar");
    let target_work = PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", target_work.id));
    let target_sidebar =
        PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", target_sidebar.id));
    client.go_to_tab(2);
    wait_for_focused_client_pane(&backend, &name, &target_work);

    client.press_alt('h');
    wait_for_focused_client_pane(&backend, &name, &target_sidebar);

    client.go_to_tab(1);
    wait_for_focused_client_pane(&backend, &name, &birth_work);
    let pokes_before = poke_lines(&poke_log).len();
    let actions_before = focus_action_count(&trace_log);
    let assists_before = accepted_focus_repairs(xdg.path(), &target_work);
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
    wait_for_focus_action(&trace_log, &focus_exec_log, &target_work, actions_before);
    let assists = wait_for_accepted_focus_repair(xdg.path(), &target_work, assists_before);

    client.send_line("rimz-routed-first");
    let routed = poll_until(
        Duration::from_secs(10),
        || std::fs::read_to_string(&input_log).map_err(|err| err.to_string()),
        |contents| contents.contains("rimz-routed-first"),
        "terminal input routed to repaired work pane",
    );
    assert!(routed.contains("rimz-routed-first"));

    assert!(assists.iter().any(|record| {
        matches!(
            &record.assist,
            rimz::harness::assist_log::Assist::FocusRepair {
                outcome: rimz::harness::assist_log::FocusRepairOutcome::AcceptedUnconfirmed,
                ..
            }
        )
    }));

    client.press_alt('h');
    std::thread::sleep(Duration::from_millis(100));
    client.go_to_tab(1);
    wait_for_focused_client_pane(&backend, &name, &birth_work);
    let pokes_before_reload = poke_lines(&poke_log).len();
    let room_bin = rimz::StatePaths::under(sidebar.workspace_id.clone(), xdg.path())
        .expect("room state paths")
        .room_bin;
    backend
        .ensure_presence_plugin(&rimz::mux::PresencePluginOptions {
            session_name: name.clone(),
            workspace_id: sidebar.workspace_id.clone(),
            wasm,
            rimz_bin: room_bin,
            converge: true,
            seed_permissions: false,
            focus_key: Some("Alt+p".to_owned()),
            focus_follows_mouse: false,
            mouse_click_through: true,
        })
        .expect("reload presence plugin in place");
    wait_for_reload_baseline(&poke_log, pokes_before_reload, &birth_work);
    let pokes_before = poke_lines(&poke_log).len();
    let actions_before = focus_action_count(&trace_log);
    let assists_before = accepted_focus_repairs(xdg.path(), &target_work);
    client.go_to_tab(2);
    wait_for_switch_settled(&poke_log, pokes_before);
    wait_for_focus_action(&trace_log, &focus_exec_log, &target_work, actions_before);
    wait_for_accepted_focus_repair(xdg.path(), &target_work, assists_before);
    client.send_line("rimz-routed-after-reload");
    poll_until(
        Duration::from_secs(10),
        || std::fs::read_to_string(&input_log).map_err(|err| err.to_string()),
        |contents| contents.contains("rimz-routed-after-reload"),
        "terminal input routed after plugin reload",
    );

    client.press_alt('h');
    std::thread::sleep(Duration::from_millis(100));
    client.go_to_tab(1);
    wait_for_focused_client_pane(&backend, &name, &birth_work);
    let actions_before = focus_action_count(&trace_log);
    client.go_to_tab(2);
    drop(client);
    let detached = poll_until(
        Duration::from_secs(5),
        || client_viewed_panes(&backend, &name),
        Vec::is_empty,
        "detached client view",
    );
    assert!(detached.is_empty());
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

    let xdg = scoped_runtime_dir();
    seed_presence_permissions(xdg.path(), &wasm);
    let real_rimz = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    let name = unique_session_name("presence-cwd");
    let session = ZellijSession::attach_pty(xdg, name.clone(), true);
    let launch_cwd = TempDir::new().expect("plugin launch cwd");
    let workspace_id = WorkspaceId::parse("ws_0123456789abcdef01234567").expect("fixed id");
    let runtime = rimz::store::RuntimePaths::under(workspace_id, session.xdg.path())
        .expect("presence runtime paths");
    let stamp_path = rimz::sidebar::cache::presence_stamp_path(&runtime);
    let plugin_url = format!("file:{}", wasm.display());
    let configuration = format!(
        "workspace_id=ws_0123456789abcdef01234567,session_name={name},rimz_bin={},focus_follows_mouse=false,mouse_click_through=true",
        real_rimz.display(),
    );

    let loaded = scoped_zellij(session.xdg.path())
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
        .status()
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
fn share_web_session_enables_browser_clients_on_a_clientless_session() {
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

    let xdg = scoped_runtime_dir();
    let cwd = TempDir::new().expect("session cwd tempdir");
    let name = unique_session_name("webshare");
    create_plain_background_session(xdg.path(), &name, cwd.path(), "30");
    wait_until_session_ready(xdg.path(), &name);
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };

    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    backend
        .share_web_session(&rimz::mux::PresencePluginOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::parse("ws_0123456789abcdef01234567").expect("fixed id"),
            wasm,
            rimz_bin: crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz")),
            converge: false,
            seed_permissions: true,
            focus_key: None,
            focus_follows_mouse: false,
            mouse_click_through: true,
        })
        .expect("share web session against a clientless Zellij session");

    wait_for_web_clients_allowed(xdg.path(), &name);
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

    let xdg = scoped_runtime_dir();
    seed_presence_permissions(xdg.path(), &wasm);
    let poke_log = xdg.path().join("poke.log");
    let focus_exec_log = xdg.path().join("focus-exec.log");
    let real_rimz = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    let rimz_shim = write_poke_shim(xdg.path(), &poke_log, &real_rimz, &focus_exec_log);
    let name = unique_session_name("focuskey");
    let mut session = ZellijSession::attach_pty(xdg, name.clone(), true);

    let backend = ZellijBackend::with_runtime_dir(session.xdg.path());
    backend
        .ensure_presence_plugin(&rimz::mux::PresencePluginOptions {
            session_name: name.clone(),
            workspace_id: WorkspaceId::parse("ws_0123456789abcdef01234567").expect("fixed id"),
            wasm,
            rimz_bin: rimz_shim,
            converge: false,
            seed_permissions: false,
            focus_key: Some("Alt+p".to_owned()),
            focus_follows_mouse: false,
            mouse_click_through: true,
        })
        .expect("pipe load against a live session");

    wait_for_poke_lines(&poke_log, 1);

    // Regression shape: the plugin loads from the test process cwd, then the
    // user presses the key from a pane with a different cwd. Url-targeted
    // keybinds miss that running instance; id-targeted keybinds reach it.
    let before: BTreeSet<u64> = work_pane_geometry(session.xdg.path(), &name)
        .into_iter()
        .map(|pane| pane.id)
        .collect();
    let pane_cwd = TempDir::new().expect("different cwd tempdir");
    spawn_sleep_pane(session.xdg.path(), &name, pane_cwd.path());
    let work_pane = work_pane_geometry(session.xdg.path(), &name)
        .into_iter()
        .find(|pane| !before.contains(&pane.id))
        .unwrap_or_else(|| panic!("new different-cwd pane not found; before ids were {before:?}"));
    let session_xdg = session.xdg.path().to_path_buf();
    focus_attached_client_pane_until(
        &session_xdg,
        &name,
        work_pane.id,
        "different-cwd focus-key source pane",
        || session.press_alt('l'),
    );

    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let focus_line = loop {
        session.press_alt('p');
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
