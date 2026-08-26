use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use rimz::ids::{MuxName, PaneId};
use rimz::mux::{
    BRACKET_PASTE_CLOSE, BRACKET_PASTE_OPEN, MuxBackend, NamedKey, PaneListOptions,
    PaneReadConsistency, SplitPaneOptions, SplitPlacement, SplitTarget, ZellijBackend,
};
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
    record_known_workspace_session(
        &env.state_root(),
        &opts.workspace_id,
        &workspace_root,
        &name,
    );
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
        .capture_pane(&target, Some(1), true)
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
                .send_keys(&pane_id, &shell_marker_command)
                .map_err(|err| err.to_string())?;
            backend
                .send_key(&pane_id, NamedKey::Enter)
                .map_err(|err| err.to_string())?;
            std::fs::read(&shell_ready).map_err(|err| err.to_string())
        },
        |bytes| bytes == b"ready",
        "default shell readiness marker",
    );
    backend
        .send_keys(
            &pane_id,
            &format!(
                "stty raw -echo; printf ready > {}; dd bs=1 count={} of={} 2>/dev/null; stty sane",
                reader_ready.display(),
                expected.len(),
                pasted_bytes.display(),
            ),
        )
        .expect("type raw paste reader");
    backend
        .send_key(&pane_id, NamedKey::Enter)
        .expect("start raw paste reader");
    std::fs::write(&shell_release, b"release").expect("release synchronized shell");
    poll_until(
        Duration::from_secs(10),
        || std::fs::read(&reader_ready).map_err(|err| err.to_string()),
        |bytes| bytes == b"ready",
        "raw paste reader readiness marker",
    );

    backend.paste_text(&pane_id, payload).expect("paste_text");
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
            &format!(
                "stty raw -echo; dd bs=1 count=4 of={} 2>/dev/null; stty sane",
                key_bytes.display()
            ),
        )
        .expect("type raw key reader");
    backend
        .send_key(&pane_id, NamedKey::Enter)
        .expect("start raw key reader");
    // Queue writes in PTY order; stty's TCSANOW mode switch preserves pending input, so keep this delay-free.
    backend
        .send_key(&pane_id, NamedKey::Escape)
        .expect("send escape");
    backend
        .send_key(&pane_id, NamedKey::ShiftTab)
        .expect("send shift-tab");

    let bytes = poll_until(
        Duration::from_secs(10),
        || std::fs::read(&key_bytes).map_err(|err| err.to_string()),
        |bytes| bytes.len() == 4,
        "named keys in the raw reader",
    );
    assert_eq!(bytes, b"\x1b\x1b[Z");
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
