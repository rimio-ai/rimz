#![allow(clippy::print_stdout, clippy::print_stderr)]

use super::support::*;

#[test]
fn ensure_session_applies_room_contract() {
    require_tmux!();
    let server = TmuxServer::new();
    assert_eq!(
        server.backend.list_sessions().expect("empty list_sessions"),
        Vec::<String>::new(),
        "a fresh private socket starts with no sessions",
    );
    let cwd = TempDir::new().expect("cwd tempdir");
    let workspace_id = WorkspaceId::from_project_root(cwd.path());
    let runtime = RuntimePaths::under(workspace_id.clone(), &cwd.path().join("runtime"))
        .expect("runtime paths");
    runtime.ensure_dirs().expect("runtime dirs");
    let shim_dir = TempDir::new().expect("shim tempdir");
    let marker = cwd.path().join("copilot-env.txt");
    let copilot = shim_dir.path().join("copilot");
    std::fs::write(
        &copilot,
        format!(
            "#!/bin/sh\nprintf '%s\\n%s\\n' \"$COPILOT_OTEL_FILE_EXPORTER_PATH\" \"$OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT\" > '{}'\n",
            marker.display()
        ),
    )
    .expect("write copilot shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&copilot, std::fs::Permissions::from_mode(0o755))
            .expect("chmod copilot shim");
    }
    let extra_env = BTreeMap::from([
        (
            "COPILOT_OTEL_FILE_EXPORTER_PATH".to_owned(),
            runtime.copilot_otel_path().to_string_lossy().into_owned(),
        ),
        (
            "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT".to_owned(),
            "false".to_owned(),
        ),
    ]);
    let opts = SessionOptions {
        truecolor: true,
        extra_env,
        ..session_opts(
            "rimz-options",
            workspace_id.clone(),
            cwd.path(),
            cwd.path(),
            None,
        )
    };
    server.backend.ensure_session(&opts).expect("ensure");
    assert_eq!(server.show_option(&["-s"], "escape-time"), "0");
    assert_eq!(server.show_option(&["-s"], "extended-keys"), "on");
    let terminal_features = server.show_option(&["-s"], "terminal-features");
    assert!(
        terminal_features.contains("extkeys"),
        "extkeys terminal-feature lets Alt+Enter reach agents as CSI-u",
    );
    assert!(
        terminal_features.contains("sync"),
        "sync terminal-feature lets tmux forward atomic redraws for flicker-free pets and TUIs",
    );
    let root_keys = server.list_keys("root");
    assert!(
        root_keys
            .lines()
            .any(|line| line.contains("S-Enter") && line.contains("[13;2u")),
        "S-Enter must inject CSI-u soft newline: {root_keys}"
    );
    assert!(
        root_keys
            .lines()
            .any(|line| line.contains("M-Enter") && line.contains("[13;3u")),
        "M-Enter must inject CSI-u soft newline: {root_keys}"
    );
    assert!(
        server
            .show_option(&["-s"], "user-keys[240]")
            .contains("27u"),
        "User240 must name modifier-less CSI-u Escape",
    );
    assert!(
        root_keys
            .lines()
            .any(|line| line.contains("User240") && line.contains("send-keys Escape")),
        "User240 must normalize modifier-less CSI-u to Escape: {root_keys}",
    );
    assert_eq!(server.show_option(&["-t", "rimz-options"], "mouse"), "on");
    assert_eq!(
        server.show_option(&["-w", "-t", "rimz-options"], "allow-passthrough"),
        "on",
    );
    let listed = server.backend.list_sessions().expect("list_sessions");
    assert!(
        listed.iter().any(|s| s == "rimz-options"),
        "expected `rimz-options` in {listed:?}",
    );
    let expected = cwd.path().display().to_string();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut current = server.pane_current_path("rimz-options");
    while current != expected && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(50));
        current = server.pane_current_path("rimz-options");
    }
    assert_eq!(current, expected);
    let pin = show_session_environment(&server, "rimz-options", rimz::workspace::ENV_WORKSPACE_ID);
    assert_eq!(
        pin,
        format!("{}={}", rimz::workspace::ENV_WORKSPACE_ID, workspace_id,),
    );
    let root = show_session_environment(&server, "rimz-options", rimz::workspace::ENV_PROJECT_ROOT);
    assert_eq!(
        root,
        format!(
            "{}={}",
            rimz::workspace::ENV_PROJECT_ROOT,
            cwd.path().display(),
        ),
    );
    assert_eq!(
        show_session_environment(&server, "rimz-options", "COLORTERM"),
        "COLORTERM=truecolor",
    );
    assert_eq!(
        show_session_environment(&server, "rimz-options", "COPILOT_OTEL_FILE_EXPORTER_PATH"),
        format!(
            "COPILOT_OTEL_FILE_EXPORTER_PATH={}",
            runtime.copilot_otel_path().display()
        ),
    );
    assert_eq!(
        show_session_environment(
            &server,
            "rimz-options",
            "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT"
        ),
        "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT=false",
    );

    let work_pane = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("rimz-options".to_owned()),
            ..Default::default()
        })
        .expect("list work pane")
        .panes[0]
        .pane_id
        .clone();
    server
        .backend
        .send_keys(&work_pane, copilot.to_string_lossy().as_ref())
        .expect("type direct copilot shim");
    server
        .backend
        .send_key(&work_pane, NamedKey::Enter)
        .expect("run direct copilot shim");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        std::fs::read_to_string(&marker).expect("direct copilot shim output"),
        format!("{}\nfalse\n", runtime.copilot_otel_path().display()),
    );

    let mut reasserted = opts;
    reasserted
        .extra_env
        .insert("RIMZ_TEST_REASSERT".to_owned(), "after-birth".to_owned());
    server
        .backend
        .ensure_session(&reasserted)
        .expect("reassert existing session env");
    assert_eq!(
        show_session_environment(&server, "rimz-options", "RIMZ_TEST_REASSERT"),
        "RIMZ_TEST_REASSERT=after-birth",
    );
}

/// `focus_pane` lands cross-window: tmux's `select-pane` activates within its
/// window only, so the backend batches `select-window` (a pane id resolves as
/// a window target to the window holding it) before `select-pane`. The
/// session's current window must follow the jump.

#[test]
fn list_panes_metadata_and_cross_window_focus_round_trip() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("rimz-jump");
    // A second window, opened without focus so the first stays current.
    server.tmux(&["new-window", "-d", "-t", "rimz-jump", "-n", "second", "sh"]);
    let deadline = Instant::now() + Duration::from_secs(5);
    let target = loop {
        let pane = server
            .backend
            .list_panes(PaneListOptions {
                session_name: Some("rimz-jump".to_owned()),
                ..Default::default()
            })
            .expect("list_panes")
            .panes
            .into_iter()
            .find(|pane| pane.view_name.as_deref() == Some("second"))
            .expect("the second window's pane");
        if pane.command.as_deref() == Some("sh") || Instant::now() >= deadline {
            break pane;
        }
        thread::sleep(Duration::from_millis(25));
    };
    let window = target
        .view_id
        .clone()
        .expect("tmux panes carry a window id");
    assert_eq!(target.pane_id.mux(), MuxName::Tmux);
    assert!(target.pane_id.raw().starts_with('%'));
    assert_eq!(target.session_name, "rimz-jump");
    assert_eq!(target.view_name.as_deref(), Some("second"));
    assert_eq!(target.command.as_deref(), Some("sh"));
    assert!(target.cwd.as_deref().is_some_and(|cwd| !cwd.is_empty()));
    assert_ne!(
        server.display("rimz-jump", "#{window_id}"),
        window,
        "the second window must start out not current",
    );
    server
        .backend
        .focus_pane(&target.pane_id, None)
        .expect("focus_pane");
    assert_eq!(
        server.display("rimz-jump", "#{window_id}"),
        window,
        "a cross-window jump must switch the session's current window",
    );
    assert_eq!(
        server.display("rimz-jump", "#{pane_id}"),
        target.pane_id.raw(),
        "and land on the target pane",
    );
}

#[test]
fn client_view_tracks_attached_client() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("focus");
    let pane_id = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("focus".to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes[0]
        .pane_id
        .clone();
    // No client attached: list-clients is empty, so the focus set is too.
    let detached = server
        .backend
        .client_view(ClientFocusOptions {
            session_name: Some("focus".to_owned()),
            ..Default::default()
        })
        .map(|view| view.viewed_panes)
        .expect("client_view detached");
    assert!(
        detached.is_empty(),
        "a detached session focuses no client panes: {detached:?}",
    );
    // Attach a client; its focused pane is the session's lone pane.
    let _client = AttachedTmuxClient::attach(&server.socket, "focus", 200, 50);
    let deadline = Instant::now() + Duration::from_secs(10);
    let focused = loop {
        let panes = server
            .backend
            .client_view(ClientFocusOptions {
                session_name: Some("focus".to_owned()),
                ..Default::default()
            })
            .map(|view| view.viewed_panes)
            .expect("client_view attached");
        if !panes.is_empty() || Instant::now() >= deadline {
            break panes;
        }
        thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(
        focused,
        vec![pane_id],
        "an attached client focuses the session's lone pane: {focused:?}",
    );
}
