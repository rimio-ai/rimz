use std::path::{Path, PathBuf};
use std::time::Duration;

use rimz::ids::{MuxName, PaneId, WorkspaceId};
use rimz::mux::MuxBackend;
use tempfile::TempDir;

use super::presence::{presence_wasm_artifact, seed_presence_permissions, sh_quote};
use super::support::*;

/// Gap between key writes: a chord sent right behind another key can be read
/// as Esc plus the key, and each guarded key is one pipe to the replica.
const KEY_GAP: Duration = Duration::from_millis(400);

fn press(client: &mut AttachedClient, keys: &[&[u8]]) {
    for key in keys {
        client.send(key);
        std::thread::sleep(KEY_GAP);
    }
}

fn pane(id: u64) -> PaneId {
    PaneId::from_parts(MuxName::Zellij, format!("terminal_{id}"))
}

fn sidebar_geometry(xdg: &Path, session: &str) -> PaneGeometry {
    raw_sidebar_pane(xdg, session).geometry()
}

fn work_panes_after(xdg: &Path, session: &str, count: usize, context: &str) -> Vec<PaneGeometry> {
    poll_until(
        SPAWN_TIMEOUT,
        || Ok(work_pane_geometry(xdg, session)),
        |work| work.len() == count,
        context,
    )
}

/// A live room under the rimz keymap, with its client in locked mode and focus
/// on the sidebar, beside one work pane. A test binds every guard by name,
/// `room` first and `client` last, so the client drops before the room and no
/// directory goes before the test ends.
struct KeysRoom {
    client: AttachedClient,
    log: PathBuf,
    sidebar: PaneGeometry,
    first: PaneGeometry,
    cwd_path: PathBuf,
    _stub_dir: TempDir,
    _cwd: TempDir,
    room: LiveZellijSession,
}

/// Births the room with the presence plugin's grant seeded with all six
/// permissions, so the plugin's own split never prompts, and routes the
/// plugin's `rimz` children through a shim that logs each argv and stderr.
fn keys_room(name: &str) -> KeysRoom {
    let room = LiveZellijSession::new(name);
    let zellij_config = room.path().join(".config/zellij/config.kdl");
    std::fs::write(
        &zellij_config,
        format!(
            "{}\ndefault_mode \"locked\"\n",
            std::fs::read_to_string(&zellij_config).unwrap()
        ),
    )
    .unwrap();
    let wasm = presence_wasm_artifact();
    seed_presence_permissions(room.path(), &wasm);
    std::fs::write(room.path().join(".zshrc"), "").unwrap();
    let cwd = TempDir::new().expect("cwd");
    let cwd_path = cwd.path().canonicalize().unwrap();
    let real_rimz = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    let log = room.path().join("keys.log");
    let shim = room.path().join("rimz-key-shim");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\" 2>> '{}'\n",
            log.display(),
            real_rimz.display(),
            room.path().join("keys.err").display(),
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let mut opts = sidebar_opts(room.name(), &cwd_path, stub, 120);
    opts.workspace_id = WorkspaceId::from_project_root(&cwd_path);
    let state = rimz::StatePaths::for_project_root_under(&cwd_path, room.path()).unwrap();
    opts.runtime = rimz::RuntimePaths::for_state_under(&state, room.path());
    publish_room_bin(room.path(), &opts);
    room.backend()
        .open_sidebar(&opts, None)
        .expect("open_sidebar");
    let config = "[room]\nstyle='rimz'\n[keys.bind.normal]\n\"Alt+m\"='mode.move'\n";
    let machine: rimz::config::MachineConfig = toml::from_str(config).unwrap();
    let presence = rimz::mux::PresencePluginOptions {
        session_name: room.name().to_owned(),
        workspace_id: opts.workspace_id.clone(),
        wasm,
        rimz_bin: shim,
        keymap: machine.room_keymap(),
        project_root: cwd_path.clone(),
        session_options: Vec::new(),
    };
    let mut client = AttachedClient::attach(&room, 120, 40);
    room.backend()
        .converge_presence_plugin_for(&presence)
        .unwrap();
    let sidebar = sidebar_geometry(room.path(), room.name());
    let [first] = work_panes_after(room.path(), room.name(), 1, "birth work pane")[..] else {
        unreachable!("one work pane was awaited");
    };
    client.press_alt_until('h', &pane(sidebar.id), "sidebar");
    // `open_sidebar` births the session before the config's `default_mode`
    // could apply, and this client attaches without RimZ's `--default-mode
    // locked`, so it starts in normal: Ctrl g takes it to locked, the rimz
    // keymap's home.
    client.send(b"\x07");
    poll_until(
        SPAWN_TIMEOUT,
        || Ok(client.screen()),
        |screen| screen.contains("LOCKED"),
        "client in locked mode",
    );
    KeysRoom {
        client,
        log,
        sidebar,
        first,
        cwd_path,
        _stub_dir,
        _cwd: cwd,
        room,
    }
}

fn assert_no_permission_prompt(client: &AttachedClient) {
    let screen = client.screen();
    assert!(!screen.contains("Allow?"), "no permission prompt: {screen}");
}

/// The split key pressed on the sidebar runs `sidebar split`, which splits the
/// work pane behind it, focus in the new pane, in that pane's directory.
///
/// Can fail below Zellij 0.45.0: a focus-taking directional split there has
/// made no pane in some runs, a trunk limit of `ZellijBackend::split_pane` this
/// test inherits.
#[test]
fn split_key_on_the_sidebar_splits_the_work_pane_behind_it() {
    require_zellij!();
    let KeysRoom {
        room,
        _cwd,
        _stub_dir,
        cwd_path,
        first,
        sidebar,
        log,
        mut client,
    } = keys_room("sidebar-split");
    press(&mut client, &[b"\x1bp", b"s"]);
    let from_pane = format!("--from-pane zellij:terminal_{}", sidebar.id);
    let logged = poll_until(
        SPAWN_TIMEOUT,
        || Ok(std::fs::read_to_string(&log).unwrap_or_default()),
        |logged| {
            logged
                .lines()
                .any(|line| line.contains("sidebar split down") && line.ends_with(&from_pane))
        },
        "the split key runs `sidebar split` naming the sidebar",
    );
    let work = poll_until(
        SPAWN_TIMEOUT,
        || Ok(work_pane_geometry(room.path(), room.name())),
        |work| work.len() == 2,
        &format!(
            "split from the sidebar; ran {logged}; stderr {}",
            std::fs::read_to_string(room.path().join("keys.err")).unwrap_or_default()
        ),
    );
    assert_eq!(sidebar_geometry(room.path(), room.name()), sidebar);
    let below = work
        .iter()
        .find(|pane| pane.id != first.id)
        .copied()
        .unwrap();
    assert_eq!(below.x, first.x, "split below the work pane: {work:?}");
    assert!(below.y > first.y, "split below the work pane: {work:?}");
    client.wait_until_focused(&pane(below.id), "focus in the split pane");
    client.assert_input_reaches(&pane(below.id), "split pane");
    let marker = room.path().join("split-cwd");
    client.send_line(&format!(
        "pwd > {}",
        sh_quote(&marker.display().to_string())
    ));
    poll_until(
        SPAWN_TIMEOUT,
        || std::fs::read_to_string(&marker).map_err(|err| err.to_string()),
        |observed| observed.trim() == cwd_path.to_str().unwrap(),
        "split in the work pane's directory",
    );
    assert_no_permission_prompt(&client);
}

/// On the sidebar, close, both swaps of prefix and move mode do nothing; on a
/// work pane, split, close, and swap act natively through the plugin, and a
/// swap whose partner would be the sidebar does nothing, in both directions
/// that could reach it.
#[test]
fn guarded_keys_split_close_and_swap_work_panes_and_never_the_sidebar() {
    require_zellij!();
    let KeysRoom {
        room,
        _cwd,
        _stub_dir,
        first,
        sidebar,
        log,
        mut client,
        ..
    } = keys_room("sidebar-keys");
    press(
        &mut client,
        &[
            b"\x1bp", b"X", b"\x1bp", b"H", b"\x1bp", b"L", b"\x1bm", b"h", b"l", b"\x1b",
        ],
    );
    // The plugin handles keys in order, so once the split below lands every
    // press on the sidebar before it has been handled.
    client.press_alt_once_until('l', &pane(first.id), "first work pane");
    press(&mut client, &[b"\x1bp", b"s"]);
    let work = work_panes_after(room.path(), room.name(), 2, "split down from a work pane");
    assert_eq!(sidebar_geometry(room.path(), room.name()), sidebar);
    let below = work
        .iter()
        .find(|pane| pane.id != first.id)
        .copied()
        .unwrap();
    assert_eq!(below.x, first.x, "split below the work pane: {work:?}");
    assert!(below.y > first.y, "split below the work pane: {work:?}");
    client.wait_until_focused(&pane(below.id), "focus in the split pane");

    // A work pane splits natively through the plugin, with no prompt.
    press(&mut client, &[b"\x1bp", b"v"]);
    let work = work_panes_after(room.path(), room.name(), 3, "split right from a work pane");
    let right = work
        .iter()
        .find(|pane| ![first.id, below.id].contains(&pane.id))
        .copied()
        .unwrap();
    client.wait_until_focused(&pane(right.id), "focus in the right split");
    client.press_alt_once_until('h', &pane(below.id), "pane beside the sidebar");

    // Beside the sidebar, a left swap does nothing; a right swap does.
    let at = |id: u64| {
        work_pane_geometry(room.path(), room.name())
            .into_iter()
            .find(|pane| pane.id == id)
            .unwrap()
    };
    let swapped = |id: u64, x: u64, context: &str| {
        poll_until(SPAWN_TIMEOUT, || Ok(at(id)), |pane| pane.x == x, context)
    };
    press(&mut client, &[b"\x1bm", b"h", b"l"]);
    swapped(below.id, right.x, "right swap from beside the sidebar");
    // From the rightmost pane, a right swap has no partner and no wrap-around.
    press(&mut client, &[b"l", b"h", b"\x1b"]);
    swapped(below.id, below.x, "left swap back from the rightmost pane");
    assert_eq!(sidebar_geometry(room.path(), room.name()), sidebar);
    assert_eq!(at(right.id).x, right.x, "the right pane swapped back");

    // Close on a work pane closes it.
    press(&mut client, &[b"\x1bp", b"X"]);
    let work = work_panes_after(room.path(), room.name(), 2, "close a work pane");
    assert!(work.iter().all(|pane| pane.id != below.id), "{work:?}");
    assert_eq!(sidebar_geometry(room.path(), room.name()), sidebar);
    assert_no_permission_prompt(&client);

    // In a tab whose only pane is the sidebar, split changes nothing.
    let lone_tab = |xdg: &Path| {
        let tabs = tab_ids(xdg, room.name());
        let [_, tab] = tabs[..] else {
            panic!("two tabs: {tabs:?}");
        };
        expect_list_panes(xdg, room.name())
            .panes
            .into_iter()
            .filter(|pane| pane.tab_id == tab && pane.is_live_terminal())
            .map(|pane| pane.geometry())
            .collect::<Vec<_>>()
    };
    open_new_tab(room.path(), room.name());
    let born = poll_until(
        SPAWN_TIMEOUT,
        || Ok(expect_list_panes(room.path(), room.name())),
        |snapshot| {
            let tab = snapshot.tab_ids()[1];
            snapshot
                .panes
                .iter()
                .any(|pane| pane.tab_id == tab && pane.is_live_terminal() && !pane.is_sidebar())
        },
        "the new tab's work pane",
    );
    let tab = born.tab_ids()[1];
    let in_tab = |sidebar: bool| {
        born.panes
            .iter()
            .find(|pane| {
                pane.tab_id == tab && pane.is_live_terminal() && pane.is_sidebar() == sidebar
            })
            .unwrap_or_else(|| panic!("tab {tab} has a pane with sidebar {sidebar}"))
            .id
    };
    let (lone_sidebar, shell) = (in_tab(true), in_tab(false));
    let closed = room.action(&["close-pane", "--pane-id", &format!("terminal_{shell}")]);
    assert!(closed.status.success(), "{closed:?}");
    let alone = poll_until(
        SPAWN_TIMEOUT,
        || Ok(lone_tab(room.path())),
        |panes| panes.len() == 1,
        "the sidebar alone in its tab",
    );
    client.go_to_tab_until(2, &pane(lone_sidebar), "the lone sidebar");
    press(&mut client, &[b"\x1bp", b"s"]);
    let errors = room.path().join("keys.err");
    let from_pane = format!("--from-pane zellij:terminal_{lone_sidebar}");
    poll_until(
        SPAWN_TIMEOUT,
        || Ok(std::fs::read_to_string(&log).unwrap_or_default()),
        |logged| logged.lines().any(|line| line.ends_with(&from_pane)),
        "the split key runs `sidebar split` naming the lone sidebar",
    );
    poll_until(
        SPAWN_TIMEOUT,
        || Ok(std::fs::read_to_string(&errors).unwrap_or_default()),
        |stderr| stderr.contains("rimz: sidebar is the only pane in its view; nothing to split"),
        "the split command finished",
    );
    assert_eq!(lone_tab(room.path()), alone);
    assert_eq!(tab_ids(room.path(), room.name()).len(), 2);
    assert_no_permission_prompt(&client);
}
