use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use rimz::ids::{MuxName, PaneId};
use rimz::mux::{
    MuxBackend, PaneListOptions, PaneReadConsistency, SidebarLiveness, SplitPaneOptions,
    SplitPlacement, SplitTarget, ZellijBackend,
};
use rimz::pane::keys::{BRACKET_PASTE_CLOSE, BRACKET_PASTE_OPEN, NamedKey};
use tempfile::TempDir;

use crate::common::{CommandTimeoutExt, Env};

use super::support::*;

#[test]
fn sidebar_focus_command_targets_session_from_outside_room() {
    require_zellij!();

    let room = LiveZellijSession::new("focuscmd");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let backend = ZellijBackend::with_runtime_dir(xdg);
    let opts = sidebar_opts(&name, cwd.path(), stub, 200);
    publish_room_bin(xdg, &opts);
    backend.open_sidebar(&opts, None).expect("open_sidebar");
    wait_for_pane_count(xdg, &name, 2);

    let sidebar = raw_sidebar_pane(xdg, &name);
    let sidebar_id = sidebar.id;
    let tab_id = sidebar.tab_id;
    let work_id = expect_list_panes(xdg, &name)
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.tab_id == tab_id && !pane.is_sidebar())
        .map(|pane| pane.id)
        .expect("work pane id");

    let mut client = AttachedClient::attach(&room, 200, 50);
    let work_pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{work_id}"));
    client.press_alt_until('l', &work_pane, "fixture work pane");

    let env = Env::new();
    let workspace_root = std::path::PathBuf::from(format!("/tmp/rimz-{name}"));
    record_known_workspace_session(&env.rimz_home(), &opts.workspace_id, &workspace_root, &name);
    write_topology_cache_from_list_panes(xdg, &opts.workspace_id, &name);
    let trace = TempDir::new().expect("zellij trace tempdir");
    let trace_log = trace.path().join("zellij.log");
    let shim = trace.path().join("zellij");
    let real_zellij = which::which("zellij").expect("zellij path");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            trace_log.display(),
            real_zellij.display(),
        ),
    )
    .expect("write zellij trace shim");
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
        .expect("chmod zellij trace shim");
    let output = env
        .rimz()
        .env("XDG_RUNTIME_DIR", xdg)
        .env("XDG_CACHE_HOME", xdg)
        .env("TMPDIR", xdg)
        .env("RIMZ_ZELLIJ_BIN", &shim)
        .args([
            "--mux",
            "zellij",
            "sidebar",
            "focus",
            "--session-name",
            &name,
        ])
        .bounded_output()
        .expect("rimz sidebar focus");
    assert!(
        output.status.success(),
        "rimz sidebar focus failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let log = std::fs::read_to_string(trace_log).expect("read zellij trace");
    assert!(
        log.lines().any(|line| {
            line.contains(&format!("--session {name}"))
                && line.contains(&format!("action focus-pane-id terminal_{sidebar_id}"))
        }),
        "out-of-session focus targeted the wrong session or pane:\n{log}",
    );

    let sidebar_pane = PaneId::from_parts(MuxName::Zellij, format!("terminal_{sidebar_id}"));
    client.press_alt_until('h', &sidebar_pane, "sidebar before smart zoom");
    let output = env
        .rimz()
        .env("XDG_RUNTIME_DIR", xdg)
        .env("XDG_CACHE_HOME", xdg)
        .env("TMPDIR", xdg)
        .env("RIMZ_ZELLIJ_BIN", &shim)
        .args(["--mux", "zellij", "pane", "zoom", "--session-name", &name])
        .bounded_output()
        .expect("rimz pane zoom");
    assert!(
        output.status.success(),
        "rimz pane zoom failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    client.wait_until_focused(&work_pane, "work pane selected by smart zoom");
    poll_until(
        Duration::from_secs(5),
        || Ok(expect_list_panes(xdg, &name)),
        |snapshot| {
            snapshot
                .panes
                .iter()
                .any(|pane| pane.id == work_id && pane.is_fullscreen)
        },
        "working sibling fullscreen",
    );
    write_topology_cache_from_list_panes(xdg, &opts.workspace_id, &name);
    let mut liveness = SidebarLiveness::default();
    liveness.claimed_panes.insert(sidebar_pane.clone());
    let report = backend
        .reconcile_sidebars(&opts, &liveness)
        .expect("reconcile fullscreen tab");
    assert_eq!(report.misdocked, 0, "fullscreen geometry is not a misdock");
    assert_eq!(report.redocked, 0, "fullscreen geometry is not repaired");

    let output = env
        .rimz()
        .env("XDG_RUNTIME_DIR", xdg)
        .env("XDG_CACHE_HOME", xdg)
        .env("TMPDIR", xdg)
        .env("RIMZ_ZELLIJ_BIN", &shim)
        .args(["--mux", "zellij", "pane", "zoom", "--session-name", &name])
        .bounded_output()
        .expect("rimz pane unzoom");
    assert!(
        output.status.success(),
        "rimz pane unzoom failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    poll_until(
        Duration::from_secs(5),
        || Ok(expect_list_panes(xdg, &name)),
        |snapshot| snapshot.panes.iter().all(|pane| !pane.is_fullscreen),
        "working pane unzoomed",
    );
}
#[test]
fn split_pane_injects_env_vars() {
    require_zellij!();

    let room = LiveZellijSession::new("splitenv");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    let marker_file = cwd.path().join("rimz-env-marker");

    // Birth a live background session with one long-lived pane to split from.
    room.create_plain_background(cwd.path(), "60");
    let target = wait_for_pane_count(xdg, &name, 1)[0].pane_id.clone();
    assert!(
        !target.raw().is_empty(),
        "session should have its working pane before the split",
    );

    let mut env = BTreeMap::new();
    env.insert("RIMZ_TEST_VAR".to_owned(), "marker-rimz-env".to_owned());
    ZellijBackend::with_runtime_dir(xdg)
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: name.clone(),
                pane_id: target.clone(),
            },
            cwd: Some(cwd.path().to_string_lossy().into_owned()),
            command: Some(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                format!(
                    "printf '%s' \"$RIMZ_TEST_VAR\" > {}; sleep 5",
                    marker_file.display()
                ),
            ]),
            title: None,
            close_on_exit: false,
            env,
            placement: SplitPlacement::default(),
            focus: false,
        })
        .expect("split_pane");

    let deadline = Instant::now() + Duration::from_secs(10);
    let marker = loop {
        if let Ok(text) = std::fs::read_to_string(&marker_file)
            && !text.is_empty()
        {
            break text;
        }
        assert!(
            Instant::now() < deadline,
            "env-injected split never wrote the marker file",
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        marker, "marker-rimz-env",
        "Zellij split pane missed the injected RIMZ_TEST_VAR",
    );
    ZellijBackend::with_runtime_dir(xdg)
        .capture_pane(&target, room.name(), Some(1), true)
        .expect("capture split target with scrollback and ANSI");
}

#[test]
fn split_pane_targets_non_focused_tab_without_moving_client_focus() {
    require_zellij!();

    let room = LiveZellijSession::new("splittarget");
    let xdg = room.path();
    let name = room.name().to_owned();
    let cwd = TempDir::new().expect("cwd tempdir");
    room.create_plain_background(cwd.path(), "60");
    let mut client = AttachedClient::attach(&room, 120, 40);
    let backend = ZellijBackend::with_runtime_dir(xdg);
    let first = wait_for_pane_count(xdg, &name, 1)[0].clone();
    room.command()
        .args([
            "--session",
            &name,
            "action",
            "new-pane",
            "--direction",
            "right",
            "--",
            "sleep",
            "60",
        ])
        .bounded_output()
        .expect("second target pane");
    let target = wait_for_pane_count(xdg, &name, 2)
        .into_iter()
        .find(|pane| pane.pane_id != first.pane_id)
        .expect("second target pane");
    ZellijBackend::with_runtime_dir(xdg)
        .focus_pane(&first.pane_id, Some(&name))
        .expect("focus first pane");
    open_new_tab(xdg, &name);
    wait_for_tab_count(xdg, &name, 2);
    let active_tab_pane = wait_for_pane_count(xdg, &name, 3)
        .into_iter()
        .find(|pane| pane.pane_id != first.pane_id && pane.pane_id != target.pane_id)
        .expect("new tab pane");
    client.go_to_tab_until(2, &active_tab_pane.pane_id, "new tab pane");
    let active_tab_pane_id = active_tab_pane
        .pane_id
        .creation_ordinal()
        .expect("numeric new tab pane id")
        .to_string();
    let moved = room
        .command()
        .env("ZELLIJ_PANE_ID", active_tab_pane_id)
        .args(["--session", &name, "action", "move-tab", "left"])
        .bounded_output()
        .expect("move focused tab left");
    assert!(
        moved.status.success(),
        "move focused tab left failed: {}",
        String::from_utf8_lossy(&moved.stderr),
    );
    let target = poll_until(
        Duration::from_secs(10),
        || Ok(expect_list_panes(xdg, &name).pane_refs()),
        |panes| {
            panes
                .iter()
                .any(|pane| pane.pane_id == target.pane_id && pane.view_id != target.view_id)
        },
        "target tab moved away from its original Zellij tab position",
    )
    .into_iter()
    .find(|pane| pane.pane_id == target.pane_id)
    .expect("moved target pane");
    let first_tab = target.view_id.clone().expect("moved target tab id");
    let authoritative = backend
        .list_panes(PaneListOptions {
            session_name: Some(name.clone()),
            consistency: PaneReadConsistency::PreferAuthoritative,
            ..Default::default()
        })
        .expect("authoritative pane listing after tab move");
    assert!(authoritative.panes.iter().any(|pane| {
        pane.pane_id == target.pane_id && pane.view_id.as_deref() == Some(first_tab.as_str())
    }));
    let focused_before = wait_for_human_client_count(&backend, &name, 1).viewed_panes;
    assert_eq!(
        focused_before.len(),
        1,
        "one attached client should remain in the active tab: {focused_before:?}",
    );
    assert_ne!(
        focused_before[0], target.pane_id,
        "the target stack should be in the background tab",
    );

    backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: name.clone(),
                pane_id: target.pane_id.clone(),
            },
            cwd: Some(cwd.path().to_string_lossy().into_owned()),
            command: Some(vec!["sleep".to_owned(), "60".to_owned()]),
            title: Some("rimz-new-stack".to_owned()),
            close_on_exit: false,
            env: BTreeMap::new(),
            placement: SplitPlacement::Stacked,
            focus: false,
        })
        .expect("targeted split_pane");

    let panes = poll_until(
        Duration::from_secs(10),
        || Ok(expect_list_panes(xdg, &name).pane_refs()),
        |panes| {
            panes
                .iter()
                .filter(|pane| pane.view_id.as_deref() == Some(first_tab.as_str()))
                .count()
                >= 3
        },
        "targeted split in non-focused Zellij tab",
    );
    assert_eq!(
        panes
            .iter()
            .filter(|pane| pane.view_id.as_deref() == Some(first_tab.as_str()))
            .count(),
        3,
        "targeted split should land beside the target pane, not in the focused tab: {panes:?}",
    );
    let snapshot = expect_list_panes(xdg, &name);
    let target_id = target
        .pane_id
        .creation_ordinal()
        .expect("numeric target id");
    let target_geometry = snapshot
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == target_id)
        .expect("target pane geometry");
    let new_geometry = snapshot
        .panes
        .iter()
        .find(|pane| pane.title.as_deref() == Some("rimz-new-stack"))
        .expect("new pane geometry");
    assert_eq!(
        (new_geometry.pane_x, new_geometry.pane_columns),
        (target_geometry.pane_x, target_geometry.pane_columns),
        "stacked split should use the requested pane's column: {:?}",
        snapshot.panes,
    );
    let focused_after =
        client.wait_until_focused(&focused_before[0], "client focus after background split");
    assert_eq!(
        focused_after, focused_before,
        "targeting a background stack must not switch the attached client's tab",
    );
}

#[test]
fn doctor_kitty_probe_completes_inside_a_live_zellij_pane() {
    require_zellij!();

    let room = LiveZellijSession::new("doctorgraphics");
    let backend = ZellijBackend::with_runtime_dir(room.path());
    let version = backend.version().expect("Zellij version");
    let parsed = version
        .split_whitespace()
        .nth(1)
        .and_then(|version| {
            let mut parts = version
                .split('.')
                .filter_map(|part| part.parse::<u32>().ok());
            Some((parts.next()?, parts.next()?))
        })
        .expect("numeric Zellij major.minor version");
    if parsed < (0, 45) {
        crate::common::skip("zellij below 0.45");
        return;
    }

    let _client = AttachedClient::create_and_attach(&room, 80, 24);
    let output_dir = TempDir::new().expect("doctor output dir");
    let output_path = output_dir.path().join("doctor.json");
    let rimz = crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz"));
    let spawned = room
        .command()
        .args([
            "--session",
            room.name(),
            "action",
            "new-pane",
            "--name",
            "rimz-doctor-graphics",
            "--",
            "sh",
            "-c",
            r#""$1" --zellij doctor --json > "$2""#,
            "rimz-doctor",
        ])
        .arg(&rimz)
        .arg(&output_path)
        .bounded_output()
        .expect("spawn doctor inside Zellij pane");
    assert!(
        spawned.status.success(),
        "doctor pane spawn failed: {}",
        String::from_utf8_lossy(&spawned.stderr),
    );

    let report = poll_until(
        Duration::from_secs(10),
        || {
            let bytes = std::fs::read(&output_path).map_err(|err| err.to_string())?;
            serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|err| err.to_string())
        },
        |report| {
            report["mux"]["ready"]["capabilities"]["ready"]["kitty_graphics"]
                .as_str()
                .is_some()
        },
        "doctor kitty capability from a live Zellij pane",
    );
    let graphics = report["mux"]["ready"]["capabilities"]["ready"]["kitty_graphics"]
        .as_str()
        .expect("typed kitty graphics state");
    assert!(
        matches!(graphics, "supported" | "unsupported" | "no_reply"),
        "live 0.45+ probe must attempt the round trip: {report:#}"
    );
}

/// `paste_text` writes one bracketed paste (`ESC[200~` … `ESC[201~`) with
/// terminal-style CR line endings — the message delivery path. A raw reader
/// captures the exact PTY bytes. A leading dash also guards that the byte-write
/// path never re-reads the payload as a flag or key.
#[test]
fn paste_text_encodes_newlines_and_delivers_exact_pty_bytes() {
    require_zellij!();

    let room = LiveZellijSession::new("paste");
    let xdg = room.path();
    std::fs::write(xdg.join(".zshrc"), "# hermetic test shell\n")
        .expect("write test shell profile");
    let _client = AttachedClient::create_and_attach(&room, 80, 24);
    let backend = ZellijBackend::with_runtime_dir(xdg);
    let panes = wait_for_pane_count(xdg, room.name(), 1);
    let pane_id = panes[0].pane_id.clone();
    let marker_dir = TempDir::new().expect("paste marker tempdir");
    let shell_ready = marker_dir.path().join("shell-ready");
    let shell_release = marker_dir.path().join("shell-release");
    let reader_ready = marker_dir.path().join("reader-ready");
    let pasted_bytes = marker_dir.path().join("pasted-bytes");

    let payload = "-rf rimz-paste-marker\nsecond line";
    let expected =
        format!("{BRACKET_PASTE_OPEN}-rf rimz-paste-marker\rsecond line{BRACKET_PASTE_CLOSE}")
            .into_bytes();

    let shell_marker_command = format!(
        "printf ready > {}; while [ ! -e {} ]; do sleep 0.05; done",
        shell_ready.display(),
        shell_release.display(),
    );
    poll_until(
        Duration::from_secs(10),
        || {
            if let Ok(bytes) = std::fs::read(&shell_ready)
                && bytes == b"ready"
            {
                return Ok(bytes);
            }
            backend
                .send_keys(&pane_id, room.name(), &shell_marker_command)
                .map_err(|err| err.to_string())?;
            backend
                .send_key(&pane_id, room.name(), NamedKey::Enter)
                .map_err(|err| err.to_string())?;
            std::fs::read(&shell_ready).map_err(|err| err.to_string())
        },
        |bytes| bytes == b"ready",
        "default shell readiness marker",
    );
    backend
        .send_keys(
            &pane_id,
            room.name(),
            &format!(
                "stty raw -echo; printf ready > {}; dd bs=1 count={} of={} 2>/dev/null; stty sane",
                reader_ready.display(),
                expected.len(),
                pasted_bytes.display(),
            ),
        )
        .expect("type raw paste reader");
    backend
        .send_key(&pane_id, room.name(), NamedKey::Enter)
        .expect("start raw paste reader");
    std::fs::write(&shell_release, b"release").expect("release synchronized shell");
    poll_until(
        Duration::from_secs(10),
        || std::fs::read(&reader_ready).map_err(|err| err.to_string()),
        |bytes| bytes == b"ready",
        "raw paste reader readiness marker",
    );

    backend
        .paste_text(&pane_id, room.name(), payload)
        .expect("paste_text");
    let actual = poll_until(
        Duration::from_secs(10),
        || std::fs::read(&pasted_bytes).map_err(|err| err.to_string()),
        |bytes| bytes.len() == expected.len(),
        "exact bracketed-paste bytes",
    );
    assert_eq!(actual, expected);
}

#[test]
fn semantic_answer_keys_reach_a_live_pane() {
    require_zellij!();

    let room = LiveZellijSession::new("answerkeys");
    let _client = AttachedClient::create_and_attach(&room, 80, 24);
    let backend = ZellijBackend::with_runtime_dir(room.path());
    let panes = wait_for_pane_count(room.path(), room.name(), 1);
    let pane_id = panes[0].pane_id.clone();
    let marker_dir = TempDir::new().expect("marker dir");
    let key_bytes = marker_dir.path().join("named-key-bytes");

    backend
        .send_keys(
            &pane_id,
            room.name(),
            &format!(
                "stty raw -echo; dd bs=1 count=5 of={} 2>/dev/null; stty sane",
                key_bytes.display()
            ),
        )
        .expect("type raw key reader");
    backend
        .send_key(&pane_id, room.name(), NamedKey::Enter)
        .expect("start raw key reader");
    // Queue writes in PTY order; stty's TCSANOW mode switch preserves pending input, so keep this delay-free.
    backend
        .send_key(&pane_id, room.name(), NamedKey::Escape)
        .expect("send escape");
    backend
        .send_key(&pane_id, room.name(), NamedKey::ShiftTab)
        .expect("send shift-tab");
    backend
        .send_key(&pane_id, room.name(), "ctrl-a".parse().expect("ctrl-a key"))
        .expect("send ctrl-a");

    let bytes = poll_until(
        Duration::from_secs(10),
        || std::fs::read(&key_bytes).map_err(|err| err.to_string()),
        |bytes| bytes.len() == 5,
        "named keys in the raw reader",
    );
    assert_eq!(bytes, b"\x1b\x1b[Z\x01");
}

/// An authoritative read with no workspace, so no presence cache to merge,
/// still reports a pane's live command and cwd from Zellij's own listing.
#[test]
fn authoritative_list_panes_reports_live_command_and_cwd_without_a_cache() {
    require_zellij!();

    let room = LiveZellijSession::new("nativecmd");
    let _client = AttachedClient::create_and_attach(&room, 80, 24);
    let backend = ZellijBackend::with_runtime_dir(room.path());
    wait_for_pane_count(room.path(), room.name(), 1);
    let cwd = TempDir::new().expect("pane cwd");
    spawn_sleep_pane(room.path(), room.name(), cwd.path());

    let expected_cwd = cwd.path().canonicalize().expect("canonical pane cwd");
    let listing = poll_until(
        SPAWN_TIMEOUT,
        || {
            backend
                .list_panes(PaneListOptions {
                    session_name: Some(room.name().to_owned()),
                    consistency: PaneReadConsistency::RequireAuthoritative,
                    ..Default::default()
                })
                .map_err(|err| err.to_string())
        },
        |listing| {
            listing
                .panes
                .iter()
                .any(|pane| pane.command.as_deref() == Some("sleep 600"))
        },
        "authoritative listing reports the sleep pane's live command",
    );
    let pane = listing
        .panes
        .iter()
        .find(|pane| pane.command.as_deref() == Some("sleep 600"))
        .expect("sleep pane");
    assert_eq!(
        pane.cwd.as_deref().map(std::path::Path::new),
        Some(expected_cwd.as_path()),
        "{listing:?}",
    );
}
/// `client_view` reads each client's focused pane from `list-clients`.
/// A background session with no client focuses nothing; an attached client
/// focuses its terminal pane. Drives the hook-ingestion pane-recovery probe.
#[test]
fn client_view_tracks_the_attached_client() {
    require_zellij!();

    let room = LiveZellijSession::new("focus");
    let xdg = room.path();
    let name = room.name().to_owned();

    // Birth a background session: it exists and answers actions, but has no
    // attached client yet.
    room.create_background();

    // `--create-background` births the session without attaching, but the
    // bootstrap client that created it can still surface in `list-clients` for a
    // beat before it detaches — a window that widens under load. Poll until the
    // roster drains, then assert the steady state: a background session with no
    // client focuses nothing. A real regression (a detached session that keeps a
    // focused client) never drains and still fails here.
    let detached = wait_for_human_client_count(room.backend(), &name, 0);
    assert!(
        detached.viewed_panes.is_empty(),
        "a background session with no client focuses nothing: {detached:?}",
    );

    // Construction guarantees registration, so one immediate read is enough.
    let client = AttachedClient::attach(&room, 200, 50);
    let focused = client.view();
    let pane_id = wait_for_pane_count(xdg, &name, 1)[0].pane_id.clone();

    assert_eq!(
        focused.presence.human_clients, 1,
        "one attached human client should be registered: {focused:?}",
    );
    assert_eq!(
        focused.viewed_panes,
        vec![pane_id],
        "the attached client focuses the session's lone terminal pane: {focused:?}",
    );
}

/// `pane send` and `pane capture` on a raw id address the resolved room's
/// session even when another live session holds the same pane id and the caller
/// sits outside any pane, and refuse an id the room does not hold, or a plugin
/// pane that holds no terminal, before any write action runs.
#[test]
fn pane_send_and_capture_address_the_resolved_room_among_two_sessions() {
    require_zellij!();

    let env = Env::new();
    let other_root = env.home_root.join("other");
    std::fs::create_dir_all(&other_root).expect("mkdir other project");
    let here = env.resolve_workspace(&env.project_root).session_name;
    let other = env.resolve_workspace(&other_root).session_name;
    let room = LiveZellijSession::from_namespace(crate::common::ZellijNamespace::new(), &here);
    let xdg = room.path();
    std::fs::write(xdg.join(".zshrc"), "# hermetic test shell\n")
        .expect("write test shell profile");
    room.create_background();
    let created = room
        .command()
        .args(["attach", "--create-background", &other])
        .bounded_output()
        .expect("create second session");
    assert!(created.status.success(), "second session");
    let here_pane = wait_for_pane_count(xdg, &here, 1)[0].pane_id.clone();
    let other_pane = wait_for_pane_count(xdg, &other, 1)[0].pane_id.clone();
    assert_eq!(here_pane, other_pane, "both rooms reuse one pane id");
    let backend = room.backend();
    poll_until(
        Duration::from_secs(10),
        || {
            backend
                .capture_pane(&here_pane, &here, None, false)
                .map_err(|err| err.to_string())
        },
        |capture| !capture.raw_text.trim().is_empty(),
        "shell prompt in the resolved room",
    );

    let trace = TempDir::new().expect("zellij trace tempdir");
    let trace_log = trace.path().join("zellij.log");
    let shim = trace.path().join("zellij");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            trace_log.display(),
            which::which("zellij").expect("zellij path").display(),
        ),
    )
    .expect("write zellij trace shim");
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
        .expect("chmod zellij trace shim");
    let pane = |args: &[&str]| {
        env.rimz()
            .env("XDG_RUNTIME_DIR", xdg)
            .env("XDG_CACHE_HOME", xdg)
            .env("TMPDIR", xdg)
            .env("RIMZ_ZELLIJ_BIN", &shim)
            .arg("pane")
            .args(args)
            .bounded_output()
            .expect("run rimz pane")
    };
    let id = here_pane.to_string();

    let send = pane(&["send", "--enter", &id, "echo TWO_ROOM_MARK"]);
    assert!(
        send.status.success(),
        "send: {}",
        String::from_utf8_lossy(&send.stderr)
    );
    let ran = |text: &str| text.lines().any(|line| line.trim() == "TWO_ROOM_MARK");
    poll_until(
        Duration::from_secs(10),
        || {
            backend
                .capture_pane(&here_pane, &here, None, false)
                .map_err(|err| err.to_string())
        },
        |capture| ran(&capture.raw_text),
        "mark delivered into the resolved room",
    );
    let other_text = backend
        .capture_pane(&other_pane, &other, None, false)
        .expect("capture other room")
        .raw_text;
    assert!(!other_text.contains("TWO_ROOM_MARK"), "{other_text}");

    let capture = pane(&["capture", &id]);
    assert!(
        capture.status.success(),
        "capture: {}",
        String::from_utf8_lossy(&capture.stderr)
    );
    assert!(
        ran(&String::from_utf8_lossy(&capture.stdout)),
        "capture prints the resolved room's pane: {}",
        String::from_utf8_lossy(&capture.stdout)
    );

    let plugin_id = expect_list_panes(xdg, &here)
        .panes
        .iter()
        .find(|pane| pane.is_plugin)
        .map(|pane| pane.id)
        .expect("a plugin pane in the room");
    let plugin = PaneId::from_parts(MuxName::Zellij, format!("plugin_{plugin_id}"));
    std::fs::write(&trace_log, "").expect("reset zellij trace");
    for refused in [PaneId::from_parts(MuxName::Zellij, "terminal_99"), plugin] {
        let refusal = format!(
            "pane {refused} is not in room {here}; run `rimz pane list` to see its panes, or pass `--root <project>` to address another room"
        );
        let id = refused.to_string();
        for args in [
            &["send", "--enter", &id, "echo ABSENT_MARK"][..],
            &["capture", &id][..],
        ] {
            let output = pane(args);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert_eq!(output.status.code(), Some(1), "{args:?}: {stderr}");
            assert!(stderr.contains(&refusal), "{args:?}: {stderr}");
            assert!(
                output.stdout.is_empty(),
                "{args:?} printed {:?}",
                output.stdout
            );
        }
    }
    let log = std::fs::read_to_string(&trace_log).expect("read zellij trace");
    assert!(
        !log.lines().any(|line| {
            ["write", "write-chars", "dump-screen"]
                .iter()
                .any(|verb| line.contains(&format!("action {verb} ")))
        }),
        "a refused pane got a pane action:\n{log}"
    );
}

/// From a shell outside any pane, with two sessions live, `pane split` opens
/// its pane in the resolved room, a split anchored on a room pane (the shape
/// `agents restart` and `agents resume` pass) lands there too, and `pane detach`
/// refuses before any Zellij action runs. The other session never changes.
#[test]
fn pane_split_and_detach_name_the_room_among_two_sessions() {
    require_zellij!();

    let env = Env::new();
    let other_root = env.home_root.join("other");
    std::fs::create_dir_all(&other_root).expect("mkdir other project");
    let here = env.resolve_workspace(&env.project_root).session_name;
    let other = env.resolve_workspace(&other_root).session_name;
    let room = LiveZellijSession::from_namespace(crate::common::ZellijNamespace::new(), &here);
    let xdg = room.path();
    std::fs::write(xdg.join(".zshrc"), "# hermetic test shell\n")
        .expect("write test shell profile");
    room.create_background();
    let created = room
        .command()
        .args(["attach", "--create-background", &other])
        .bounded_output()
        .expect("create second session");
    assert!(created.status.success(), "second session");
    let here_pane = wait_for_pane_count(xdg, &here, 1)[0].pane_id.clone();
    wait_for_pane_count(xdg, &other, 1);

    let trace = TempDir::new().expect("zellij trace tempdir");
    let trace_log = trace.path().join("zellij.log");
    let shim = trace.path().join("zellij");
    std::fs::write(
        &shim,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\nexec '{}' \"$@\"\n",
            trace_log.display(),
            which::which("zellij").expect("zellij path").display(),
        ),
    )
    .expect("write zellij trace shim");
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755))
        .expect("chmod zellij trace shim");
    let pane = |verb: &str| {
        env.rimz()
            .env("XDG_RUNTIME_DIR", xdg)
            .env("XDG_CACHE_HOME", xdg)
            .env("TMPDIR", xdg)
            .env("RIMZ_ZELLIJ_BIN", &shim)
            .args(["--mux", "zellij", "pane", verb])
            .bounded_output()
            .expect("run rimz pane")
    };

    let split = pane("split");
    assert!(
        split.status.success(),
        "split: {}",
        String::from_utf8_lossy(&split.stderr)
    );
    let printed = String::from_utf8(split.stdout).unwrap();
    assert!(
        printed.starts_with("zellij:terminal_") && printed.lines().count() == 1,
        "{printed:?}"
    );
    let created = PaneId::parse(printed.trim()).unwrap();
    assert!(
        wait_for_pane_count(xdg, &here, 2)
            .iter()
            .any(|pane| pane.pane_id == created)
    );

    room.backend()
        .split_pane(SplitPaneOptions {
            target: SplitTarget::SessionPane {
                session_name: here.clone(),
                pane_id: here_pane,
            },
            focus: true,
            ..Default::default()
        })
        .expect("split beside a room pane");
    wait_for_pane_count(xdg, &here, 3);

    let detach = pane("detach");
    let stderr = String::from_utf8_lossy(&detach.stderr);
    assert_eq!(detach.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(
            "this Zellij action has no session to address; run the command from a pane inside the room"
        ),
        "{stderr}"
    );
    let log = std::fs::read_to_string(&trace_log).expect("read zellij trace");
    assert!(!log.contains("action detach"), "{log}");
    assert_eq!(expect_list_panes(xdg, &here).pane_refs().len(), 3);
    assert_eq!(
        expect_list_panes(xdg, &other).pane_refs().len(),
        1,
        "the other session gained a pane"
    );
}

/// A room pane whose command has exited stays in the session while Zellij
/// holds it, so `pane capture` still reads its last screen even though
/// `pane list` omits it.
#[test]
fn pane_capture_reads_a_held_pane_that_pane_list_omits() {
    require_zellij!();

    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let here = workspace.session_name;
    record_known_workspace_session(
        &env.rimz_home(),
        &workspace.workspace_id,
        &env.project_root,
        &here,
    );
    let room = LiveZellijSession::from_namespace(crate::common::ZellijNamespace::new(), &here);
    let xdg = room.path();
    std::fs::write(xdg.join(".zshrc"), "# hermetic test shell\n")
        .expect("write test shell profile");
    room.create_background();
    let opened = room
        .command()
        .args(["--session", &here, "action", "new-pane", "--"])
        .args(["sh", "-c", "echo HELD_MARK"])
        .bounded_output()
        .expect("open a pane whose command exits");
    assert!(
        opened.status.success(),
        "new-pane: {}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let held = poll_until(
        Duration::from_secs(10),
        || {
            list_panes(xdg, &here).map(|snapshot| {
                snapshot.panes.into_iter().find(|pane| {
                    pane.is_held
                        && pane
                            .terminal_command
                            .as_deref()
                            .is_some_and(|command| command.contains("HELD_MARK"))
                })
            })
        },
        Option::is_some,
        "the exited command's pane held",
    )
    .map(|pane| PaneId::from_parts(MuxName::Zellij, format!("terminal_{}", pane.id)))
    .expect("held pane");

    room.backend()
        .require_pane_in_session(&held, &here)
        .expect("a held pane is in the room");
    let pane = |args: &[&str]| {
        env.rimz()
            .env("XDG_RUNTIME_DIR", xdg)
            .env("XDG_CACHE_HOME", xdg)
            .env("TMPDIR", xdg)
            .arg("pane")
            .args(args)
            .bounded_output()
            .expect("run rimz pane")
    };
    let capture = pane(&["capture", &held.to_string()]);
    assert!(
        capture.status.success(),
        "capture: {}",
        String::from_utf8_lossy(&capture.stderr)
    );
    let text = String::from_utf8_lossy(&capture.stdout);
    assert!(
        text.lines().any(|line| line.trim() == "HELD_MARK"),
        "capture prints the exited command's output: {text}"
    );

    write_topology_cache_from_list_panes(xdg, &workspace.workspace_id, &here);
    let list = env
        .rimz()
        .env("XDG_RUNTIME_DIR", xdg)
        .env("XDG_CACHE_HOME", xdg)
        .env("TMPDIR", xdg)
        .args(["--mux", "zellij", "pane", "list"])
        .bounded_output()
        .expect("run rimz pane list");
    let listed = String::from_utf8_lossy(&list.stdout);
    assert!(
        list.status.success(),
        "list: {}",
        String::from_utf8_lossy(&list.stderr)
    );
    let lists = |pane: &PaneId| {
        listed
            .split_whitespace()
            .any(|word| word == pane.to_string())
    };
    let shell = expect_list_panes(xdg, &here).pane_refs()[0].pane_id.clone();
    assert!(
        lists(&shell),
        "pane list shows the live shell {shell}:\n{listed}"
    );
    assert!(
        !lists(&held),
        "pane list omits the held pane {held}:\n{listed}"
    );
}

/// A room pane another pane replaced in place stays in the session suppressed,
/// where Zellij takes a write or a screen dump for it, exits 0, and does
/// nothing. `pane send` and `pane capture` refuse it rather than report a
/// delivery that never happened, and reach it again once the replacement
/// closes.
#[test]
fn pane_send_and_capture_refuse_a_suppressed_pane_until_it_returns() {
    require_zellij!();

    let env = Env::new();
    let workspace = env.resolve_workspace(&env.project_root);
    let here = workspace.session_name;
    record_known_workspace_session(
        &env.rimz_home(),
        &workspace.workspace_id,
        &env.project_root,
        &here,
    );
    let room = LiveZellijSession::from_namespace(crate::common::ZellijNamespace::new(), &here);
    let backend = room.backend();
    // The fixture suppresses the pane with `new-pane --in-place --pane-id`,
    // which Zellij 0.44 does not have.
    let minor = backend
        .version()
        .expect("zellij version")
        .split('.')
        .nth(1)
        .and_then(|value| value.parse::<u32>().ok());
    if minor.is_none_or(|minor| minor < 45) {
        crate::common::skip("zellij below 0.45");
        return;
    }
    let xdg = room.path();
    std::fs::write(xdg.join(".zshrc"), "# hermetic test shell\n")
        .expect("write test shell profile");
    room.create_background();
    let shell = wait_for_pane_count(xdg, &here, 1)[0].pane_id.clone();
    let screen = || {
        backend
            .capture_pane(&shell, &here, None, false)
            .map_err(|err| err.to_string())
    };
    let shows = |text: &str, mark: &str| text.lines().any(|line| line.trim() == mark);
    poll_until(
        Duration::from_secs(10),
        screen,
        |capture| !capture.raw_text.trim().is_empty(),
        "shell prompt before the pane is suppressed",
    );
    let pane = |args: &[&str]| {
        env.rimz()
            .env("XDG_RUNTIME_DIR", xdg)
            .env("XDG_CACHE_HOME", xdg)
            .env("TMPDIR", xdg)
            .arg("pane")
            .args(args)
            .bounded_output()
            .expect("run rimz pane")
    };
    let id = shell.to_string();
    let raw_id = shell
        .raw()
        .strip_prefix("terminal_")
        .and_then(|id| id.parse::<u64>().ok())
        .expect("terminal pane id");
    let send = |text: &str| {
        let output = pane(&["send", "--enter", &id, text]);
        assert!(
            output.status.success(),
            "send {text:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    let suppressed = || {
        list_panes(xdg, &here).map(|snapshot| {
            snapshot
                .panes
                .into_iter()
                .find(|pane| !pane.is_plugin && pane.id == raw_id)
                .map(|pane| pane.is_suppressed)
        })
    };
    send("echo BEFORE_MARK");
    poll_until(
        Duration::from_secs(10),
        screen,
        |capture| shows(&capture.raw_text, "BEFORE_MARK"),
        "mark on the pane's screen before it is suppressed",
    );

    let replaced = room
        .command()
        .args(["--session", &here, "action", "new-pane", "--in-place"])
        .args(["--pane-id", shell.raw(), "--", "sleep", "600"])
        .bounded_output()
        .expect("replace the shell pane in place");
    assert!(
        replaced.status.success(),
        "new-pane --in-place: {}",
        String::from_utf8_lossy(&replaced.stderr)
    );
    let replacement = String::from_utf8_lossy(&replaced.stdout).trim().to_owned();
    poll_until(
        Duration::from_secs(10),
        suppressed,
        |state| *state == Some(true),
        "the replaced shell pane suppressed",
    );

    let refusal = format!(
        "pane {shell} is not in room {here}; run `rimz pane list` to see its panes, or pass `--root <project>` to address another room"
    );
    for args in [
        &["send", "--enter", &id, "echo REFUSED_MARK"][..],
        &["capture", &id][..],
    ] {
        let output = pane(args);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{args:?}: {stderr}");
        assert!(stderr.contains(&refusal), "{args:?}: {stderr}");
        assert!(
            output.stdout.is_empty(),
            "{args:?} printed {:?}",
            output.stdout
        );
    }

    let closed = room
        .command()
        .args(["--session", &here, "action", "close-pane"])
        .args(["--pane-id", &replacement])
        .bounded_output()
        .expect("close the replacement pane");
    assert!(
        closed.status.success(),
        "close-pane {replacement}: {}",
        String::from_utf8_lossy(&closed.stderr)
    );
    poll_until(
        Duration::from_secs(10),
        suppressed,
        |state| *state == Some(false),
        "the shell pane back on screen",
    );
    send("echo AFTER_MARK");
    let text = poll_until(
        Duration::from_secs(10),
        || {
            let output = pane(&["capture", &id]);
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).into_owned())
            } else {
                Err(String::from_utf8_lossy(&output.stderr).into_owned())
            }
        },
        |text| shows(text, "AFTER_MARK"),
        "text sent to the returned pane ran in it",
    );
    assert!(
        shows(&text, "BEFORE_MARK"),
        "the pane kept its screen while suppressed: {text}"
    );
    assert!(
        !text.contains("REFUSED_MARK"),
        "the refused send reached the pane: {text}"
    );
}
