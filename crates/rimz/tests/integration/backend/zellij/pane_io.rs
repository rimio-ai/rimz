use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

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

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("focuscmd");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let (_stub_dir, stub) = sidebar_command_stub();
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    let opts = sidebar_opts(&name, cwd.path(), stub, 200);
    publish_room_bin(xdg.path(), &opts);
    backend.open_sidebar(&opts, None).expect("open_sidebar");
    wait_for_pane_count(xdg.path(), &name, 2);

    let sidebar = raw_sidebar_pane(xdg.path(), &name);
    let sidebar_id = sidebar.id;
    let tab_id = sidebar.tab_id;
    let work_id = expect_list_panes(xdg.path(), &name)
        .panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.tab_id == tab_id && !pane.is_sidebar())
        .map(|pane| pane.id)
        .expect("work pane id");

    let mut client = AttachedClient::attach(xdg.path(), &name, 200, 50);
    focus_attached_client_pane_until(xdg.path(), &name, work_id, "fixture work pane", || {
        client.press_alt('l')
    });

    let env = Env::new();
    let workspace_root = std::path::PathBuf::from(format!("/tmp/rimz-{name}"));
    record_known_workspace_session(
        &env.state_root(),
        &opts.workspace_id,
        &workspace_root,
        &name,
    );
    write_topology_cache_from_list_panes(xdg.path(), &opts.workspace_id, &name);
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
        .env("XDG_RUNTIME_DIR", xdg.path())
        .env("XDG_CACHE_HOME", xdg.path())
        .env("TMPDIR", xdg.path())
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
}
#[test]
fn split_pane_injects_env_vars() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("splitenv");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    let marker_file = cwd.path().join("rimz-env-marker");

    // Birth a live background session with one long-lived pane to split from.
    create_plain_background_session(xdg.path(), &name, cwd.path(), "60");
    let target = wait_for_pane_count(xdg.path(), &name, 1)[0].pane_id.clone();
    assert!(
        !target.raw().is_empty(),
        "session should have its working pane before the split",
    );

    let mut env = BTreeMap::new();
    env.insert("RIMZ_TEST_VAR".to_owned(), "marker-rimz-env".to_owned());
    ZellijBackend::with_runtime_dir(xdg.path())
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
    ZellijBackend::with_runtime_dir(xdg.path())
        .capture_pane(&target, Some(1), true)
        .expect("capture split target with scrollback and ANSI");
}

#[test]
fn split_pane_targets_non_focused_tab_without_moving_client_focus() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("splittarget");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };
    let cwd = TempDir::new().expect("cwd tempdir");
    create_plain_background_session(xdg.path(), &name, cwd.path(), "60");
    let mut client = AttachedClient::attach(xdg.path(), &name, 120, 40);
    let backend = ZellijBackend::with_runtime_dir(xdg.path());
    let first = wait_for_pane_count(xdg.path(), &name, 1)[0].clone();
    scoped_zellij(xdg.path())
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
    let target = wait_for_pane_count(xdg.path(), &name, 2)
        .into_iter()
        .find(|pane| pane.pane_id != first.pane_id)
        .expect("second target pane");
    ZellijBackend::with_runtime_dir(xdg.path())
        .focus_pane(&first.pane_id, Some(&name))
        .expect("focus first pane");
    open_new_tab(xdg.path(), &name);
    wait_for_tab_count(xdg.path(), &name, 2);
    let active_tab_pane = wait_for_pane_count(xdg.path(), &name, 3)
        .into_iter()
        .find(|pane| pane.pane_id != first.pane_id && pane.pane_id != target.pane_id)
        .expect("new tab pane");
    let active_tab_pane_id = active_tab_pane
        .pane_id
        .creation_ordinal()
        .expect("numeric new tab pane id");
    focus_attached_client_pane_until(
        xdg.path(),
        &name,
        active_tab_pane_id,
        "new tab pane",
        || client.go_to_tab(2),
    );
    let active_tab_pane_id = active_tab_pane_id.to_string();
    let moved = scoped_zellij(xdg.path())
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
        || Ok(expect_list_panes(xdg.path(), &name).pane_refs()),
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
    let focused_before = wait_for_client_view_count(xdg.path(), &name, 1);
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
            command: Some(vec!["sleep".to_owned(), "5".to_owned()]),
            title: None,
            close_on_exit: false,
            env: BTreeMap::new(),
            placement: SplitPlacement::Stacked,
            focus: false,
        })
        .expect("targeted split_pane");

    let panes = poll_until(
        Duration::from_secs(10),
        || Ok(expect_list_panes(xdg.path(), &name).pane_refs()),
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
    let snapshot = expect_list_panes(xdg.path(), &name);
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
        .find(|pane| pane.terminal_command.as_deref() == Some("sleep 5"))
        .expect("new pane geometry");
    assert_eq!(
        (new_geometry.pane_x, new_geometry.pane_columns),
        (target_geometry.pane_x, target_geometry.pane_columns),
        "stacked split should use the requested pane's column: {:?}",
        snapshot.panes,
    );
    let focused_after = wait_for_focused_client_pane(&backend, &name, &focused_before[0]);
    assert_eq!(
        focused_after, focused_before,
        "targeting a background stack must not switch the attached client's tab",
    );
}

/// `paste_text` writes one bracketed paste (`ESC[200~` … `ESC[201~`) wrapping
/// the payload as a raw decimal byte list — the message delivery path. A raw
/// reader captures the exact PTY bytes. A leading dash is the regression guard:
/// the byte-write path must never re-read the payload as a flag or key.
#[test]
fn paste_text_delivers_the_literal_payload() {
    require_zellij!();

    let xdg = scoped_runtime_dir();
    std::fs::write(xdg.path().join(".zshrc"), "# hermetic test shell\n")
        .expect("write test shell profile");
    let session = ZellijSession::attach_pty(xdg, unique_session_name("paste"), true);
    let backend = ZellijBackend::with_runtime_dir(session.xdg.path());
    let panes = wait_for_pane_count(session.xdg.path(), &session.name, 1);
    let pane_id = panes[0].pane_id.clone();
    let marker_dir = TempDir::new().expect("paste marker tempdir");
    let shell_ready = marker_dir.path().join("shell-ready");
    let shell_release = marker_dir.path().join("shell-release");
    let reader_ready = marker_dir.path().join("reader-ready");
    let pasted_bytes = marker_dir.path().join("pasted-bytes");

    let payload = "-rf rimz-paste-marker";
    let expected = format!("{BRACKET_PASTE_OPEN}{payload}{BRACKET_PASTE_CLOSE}").into_bytes();

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

    let session = ZellijSession::spawn(unique_session_name("answerkeys"));
    let backend = ZellijBackend::with_runtime_dir(session.xdg.path());
    let panes = wait_for_pane_count(session.xdg.path(), &session.name, 1);
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

    let xdg = scoped_runtime_dir();
    let name = unique_session_name("focus");
    let _cleanup = ScopedSessionCleanup {
        name: name.clone(),
        xdg: xdg.path().to_path_buf(),
    };

    // Birth a background session: it exists and answers actions, but has no
    // attached client yet.
    let created = scoped_zellij(xdg.path())
        .args(["attach", "--create-background", &name])
        .bounded_output()
        .expect("attach --create-background");
    assert!(
        created.status.success(),
        "create-background failed: {}",
        String::from_utf8_lossy(&created.stderr),
    );
    wait_until_session_ready(xdg.path(), &name);

    // `--create-background` births the session without attaching, but the
    // bootstrap client that created it can still surface in `list-clients` for a
    // beat before it detaches — a window that widens under load. Poll until the
    // roster drains, then assert the steady state: a background session with no
    // client focuses nothing. A real regression (a detached session that keeps a
    // focused client) never drains and still fails here.
    let detached = wait_for_client_view_count(xdg.path(), &name, 0);
    assert!(
        detached.is_empty(),
        "a background session with no client focuses nothing: {detached:?}",
    );

    // Attach a client; its focused terminal pane is now reported.
    let _client = AttachedClient::attach(xdg.path(), &name, 200, 50);
    let focused = wait_for_client_view_count(xdg.path(), &name, 1);
    let pane_id = wait_for_pane_count(xdg.path(), &name, 1)[0].pane_id.clone();

    assert_eq!(
        focused.len(),
        1,
        "one attached client focuses one pane: {focused:?}",
    );
    assert_eq!(
        focused[0], pane_id,
        "the attached client focuses the session's lone terminal pane: {focused:?}",
    );
}
