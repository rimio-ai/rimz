use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rimz::ids::WorkspaceId;
use rimz::mux::{MuxBackend, ZellijBackend, zellij};
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
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    let before = loop {
        if let Ok(bytes) = std::fs::read(&stamp_path)
            && let Ok(stamp) = serde_json::from_slice::<rimz::sidebar::cache::PresenceStamp>(&bytes)
        {
            break stamp.written_at_ms;
        }
        assert!(
            Instant::now() <= deadline,
            "presence plugin never wrote {}",
            stamp_path.display(),
        );
        std::thread::sleep(Duration::from_millis(50));
    };

    launch_cwd.close().expect("delete plugin launch cwd");
    std::thread::sleep(Duration::from_millis(61_000));

    let bytes = std::fs::read(&stamp_path).expect("presence stamp survives launch cwd deletion");
    let after: rimz::sidebar::cache::PresenceStamp =
        serde_json::from_slice(&bytes).expect("presence stamp JSON");
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
    let tab_id = tab_ids(session.xdg.path(), &name)
        .into_iter()
        .next()
        .expect("session has a tab");
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
    focus_nonplugin_pane_until(
        session.xdg.path(),
        &name,
        tab_id,
        work_pane.id,
        "different-cwd focus-key source pane",
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
