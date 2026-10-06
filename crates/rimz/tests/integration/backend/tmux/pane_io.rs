#![allow(clippy::print_stdout)]

use super::support::*;

#[test]
fn fullscreen_toggle_targets_the_requested_pane() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("zoom");
    server.tmux(&["split-window", "-d", "-t", "zoom", "sleep", "60"]);
    assert_eq!(list_session_panes(&server, "zoom").len(), 2);
    let pane = list_session_panes(&server, "zoom")[0].pane_id.clone();

    server
        .backend
        .toggle_fullscreen(&pane, Some("zoom"))
        .expect("zoom pane");
    assert_eq!(server.display("zoom", "#{window_zoomed_flag}"), "1");

    server
        .backend
        .toggle_fullscreen(&pane, Some("zoom"))
        .expect("unzoom pane");
    assert_eq!(server.display("zoom", "#{window_zoomed_flag}"), "0");
}

#[test]
fn split_pane_injects_env_vars() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("split");
    let focused = server.display("split", "#{pane_id}");
    let mut env = BTreeMap::new();
    env.insert("RIMZ_TEST_VAR".to_owned(), "marker-rimz-env".to_owned());
    let created = server
        .backend
        .split_pane(SplitPaneOptions {
            target: SplitTarget::Ambient,
            cwd: None,
            command: Some(vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "printf RIMZ_TEST_VAR=$RIMZ_TEST_VAR; sleep 5".to_owned(),
            ]),
            title: None,
            close_on_exit: false,
            env,
            placement: SplitPlacement::default(),
            focus: false,
        })
        .expect("split_pane");
    assert_eq!(
        server.display("split", "#{pane_id}"),
        focused,
        "an unfocused split must leave the active pane unchanged",
    );
    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("split".to_owned()),
            ..Default::default()
        })
        .expect("list_panes after split")
        .panes;
    assert_eq!(
        panes.len(),
        2,
        "expected 2 panes after split, got {panes:?}"
    );
    let new_pane = panes
        .iter()
        .find(|p| p.pane_id.raw() != "%0")
        .expect("split created a new pane id");
    assert_eq!(created, Some(new_pane.pane_id.clone()));
    assert!(
        new_pane
            .spawn_command
            .as_deref()
            .is_some_and(|command| command.contains("printf RIMZ_TEST_VAR")),
        "split pane should expose its birth command, got {new_pane:?}",
    );
    let capture = capture_pane_until(
        &server.backend,
        &new_pane.pane_id,
        "marker-rimz-env",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains("marker-rimz-env"),
        "split-pane should expose RIMZ_TEST_VAR; capture was: {capture:?}",
    );
}

#[test]
fn floating_panes_are_classified_and_closed_with_their_view() {
    require_tmux!();
    let server = TmuxServer::new();
    if !server.supports_floating_panes() {
        crate::common::skip("tmux predates 3.7 floating panes");
        return;
    }

    server.ensure_with_shell("floating");
    let anchor = list_session_panes(&server, "floating")[0].pane_id.clone();
    server.tmux(&["new-pane", "-d", "-t", anchor.raw(), "sleep", "120"]);

    let floating = list_session_panes(&server, "floating")
        .into_iter()
        .find(|pane| pane.is_floating)
        .expect("tmux 3.7 floating pane is classified");
    assert_ne!(floating.pane_id, anchor);

    assert_eq!(
        server
            .backend
            .close_view_floating_panes("floating", &anchor)
            .expect("close floating panes"),
        vec![floating.pane_id],
    );
    assert!(
        list_session_panes(&server, "floating")
            .iter()
            .all(|pane| !pane.is_floating)
    );
}

/// `send_keys`, `send_key`, `paste_text`, and `capture_pane` round-trip through
/// a live pane.

#[test]
fn pane_io_round_trips_keys_named_keys_and_bracketed_paste() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("io");
    let panes = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("io".to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes;
    let pane_id = panes[0].pane_id.clone();
    server
        .backend
        .send_keys(&pane_id, ANY_SESSION, "printf rimz-marker-io\n")
        .expect("send_keys");
    let capture = capture_pane_until(
        &server.backend,
        &pane_id,
        "rimz-marker-io",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains("rimz-marker-io"),
        "expected marker in capture, got: {capture:?}",
    );
    server
        .backend
        .send_keys(&pane_id, ANY_SESSION, "printf rimz-marker-key")
        .expect("send_keys");
    server
        .backend
        .send_key(&pane_id, ANY_SESSION, NamedKey::Enter)
        .expect("send_key");
    let capture = capture_pane_until(
        &server.backend,
        &pane_id,
        "rimz-marker-key",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains("rimz-marker-key"),
        "expected marker in capture, got: {capture:?}",
    );
    let key_bytes = server._tempdir.path().join("named-key-bytes");
    server
        .backend
        .send_keys(
            &pane_id, ANY_SESSION,
            &format!(
                "stty raw -echo; dd bs=1 count=4 of={} 2>/dev/null; stty sane; printf '\\nrimz-raw-reader-ready\\n'",
                key_bytes.display()
            ),
        )
        .expect("type raw key reader");
    server
        .backend
        .send_key(&pane_id, ANY_SESSION, NamedKey::Enter)
        .expect("start raw key reader");
    thread::sleep(Duration::from_millis(100));
    server
        .backend
        .send_key(&pane_id, ANY_SESSION, NamedKey::Escape)
        .expect("send escape");
    server
        .backend
        .send_key(&pane_id, ANY_SESSION, NamedKey::ShiftTab)
        .expect("send shift-tab");
    let deadline = Instant::now() + Duration::from_secs(2);
    let bytes = loop {
        if let Ok(bytes) = std::fs::read(&key_bytes)
            && bytes.len() == 4
        {
            break bytes;
        }
        assert!(
            Instant::now() < deadline,
            "named keys did not reach tmux pane"
        );
        thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(bytes, b"\x1b\x1b[Z");
    let capture = capture_pane_until(
        &server.backend,
        &pane_id,
        "rimz-raw-reader-ready",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains("rimz-raw-reader-ready"),
        "the shell should restore cooked mode before the paste; capture was: {capture:?}",
    );
    // Leading dash guards the `send-keys -l --` spelling: payload bytes must not
    // be re-read as tmux flags or key names.
    let payload = "-rf rimz-paste-marker";
    server
        .backend
        .paste_text(&pane_id, ANY_SESSION, payload)
        .expect("paste_text");
    let capture = capture_pane_until(
        &server.backend,
        &pane_id,
        "rimz-paste-marker",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains(payload),
        "the pasted payload should arrive contiguous and byte-safe, got: {capture:?}",
    );
}

/// Presence watch stays writable as the sole client so send-keys still works.

#[test]
fn send_keys_works_with_presence_watch_as_only_client() {
    require_tmux!();
    let server = TmuxServer::new();
    server.ensure_with_shell("headless");
    let pane_id = server
        .backend
        .list_panes(PaneListOptions {
            session_name: Some("headless".to_owned()),
            ..Default::default()
        })
        .expect("list_panes")
        .panes[0]
        .pane_id
        .clone();
    let _watch = rimz::mux::tmux::PresenceWatch::attach(&server.socket, "headless")
        .expect("attach control client");
    server.wait_for_control_client("headless");
    server
        .backend
        .send_keys(&pane_id, ANY_SESSION, "printf rimz-watch-send\n")
        .expect("send_keys under presence watch");
    let capture = capture_pane_until(
        &server.backend,
        &pane_id,
        "rimz-watch-send",
        Duration::from_secs(2),
    );
    assert!(
        capture.contains("rimz-watch-send"),
        "send_keys should work when the presence watch is the only client, got: {capture:?}",
    );
}

/// Two rooms on one managed server: the fixture project's room `here` and a
/// sibling project's room `other`, each with one shell pane.
struct TwoRooms {
    env: Env,
    server: TmuxServer,
    here: String,
    other: String,
    here_pane: PaneId,
    other_pane: PaneId,
}

impl TwoRooms {
    fn new() -> Self {
        let env = Env::new();
        let other_root = env.home_root.join("other");
        std::fs::create_dir_all(&other_root).expect("mkdir other project");
        let here = env.resolve_workspace(&env.project_root).session_name;
        let other = env.resolve_workspace(&other_root).session_name;
        let server = TmuxServer::in_runtime_root(&env.runtime_root);
        server.ensure_with_shell(&here);
        server.ensure_with_shell(&other);
        let here_pane = list_session_panes(&server, &here)[0].pane_id.clone();
        let other_pane = list_session_panes(&server, &other)[0].pane_id.clone();
        Self {
            env,
            server,
            here,
            other,
            here_pane,
            other_pane,
        }
    }

    fn pane(&self, args: &[&str]) -> std::process::Output {
        self.env
            .rimz()
            .arg("pane")
            .args(args)
            .bounded_output()
            .expect("run rimz pane")
    }

    fn refusal(&self, pane: &PaneId) -> String {
        format!(
            "pane {pane} is not in room {}; run `rimz pane list` to see its panes, or pass `--root <project>` to address another room",
            self.here
        )
    }
}

#[test]
fn pane_send_and_capture_refuse_a_pane_from_another_room() {
    require_tmux!();
    let rooms = TwoRooms::new();
    let other = rooms.other_pane.to_string();

    let send = rooms.pane(&["send", "--enter", &other, "echo CROSS_ROOM_MARK"]);
    let capture = rooms.pane(&["capture", &other]);

    for (verb, output) in [("send", &send), ("capture", &capture)] {
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{verb}: {stderr}");
        assert!(
            stderr.contains(&rooms.refusal(&rooms.other_pane)),
            "{verb} names the resolved room: {stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "{verb} printed {:?}",
            output.stdout
        );
    }
    thread::sleep(Duration::from_millis(300));
    let text = rooms
        .server
        .backend
        .capture_pane(&rooms.other_pane, ANY_SESSION, None, false)
        .expect("capture other room")
        .raw_text;
    assert!(
        !text.contains("CROSS_ROOM_MARK"),
        "other room was written: {text}"
    );
}

/// From a shell outside any pane, `pane split` opens its pane in the resolved
/// room although the other session is the one tmux picks when no target is
/// named.
#[test]
fn pane_split_outside_a_pane_opens_in_the_resolved_room() {
    require_tmux!();
    let rooms = TwoRooms::new();
    assert_eq!(
        rooms
            .server
            .stdout(&["display-message", "-p", "#{session_name}"]),
        rooms.other,
        "the other session must be tmux's default target",
    );

    let split = rooms.pane(&["split"]);
    assert!(
        split.status.success(),
        "split: {}",
        String::from_utf8_lossy(&split.stderr)
    );

    assert_eq!(rooms.server.wait_for_panes(&rooms.here, 2).len(), 2);
    assert_eq!(
        list_session_panes(&rooms.server, &rooms.other).len(),
        1,
        "the other session gained a pane"
    );
}

#[test]
fn pane_send_delivers_text_keys_then_enter_to_a_pane_in_the_room() {
    require_tmux!();
    let rooms = TwoRooms::new();
    let here = rooms.here_pane.to_string();

    let send = rooms.pane(&[
        "send",
        "--key",
        "backspace",
        "--enter",
        &here,
        "echo ORDER_MARKX",
    ]);
    assert!(
        send.status.success(),
        "send: {}",
        String::from_utf8_lossy(&send.stderr)
    );
    let ran = |text: &str| text.lines().any(|line| line.trim() == "ORDER_MARK");
    let text = capture_pane_until(
        &rooms.server.backend,
        &rooms.here_pane,
        "ORDER_MARK\n",
        Duration::from_secs(2),
    );
    assert!(ran(&text), "text, backspace, then Enter: {text:?}");

    let capture = rooms.pane(&["capture", &here]);
    assert!(
        capture.status.success(),
        "capture: {}",
        String::from_utf8_lossy(&capture.stderr)
    );
    assert!(ran(&String::from_utf8_lossy(&capture.stdout)));
}

#[test]
fn pane_send_refuses_an_agent_whose_pane_left_the_room() {
    require_tmux!();
    let rooms = TwoRooms::new();
    let gone = PaneId::from_parts(MuxName::Tmux, "%99");
    let workspace = rooms.env.resolve_workspace(&rooms.env.project_root);
    rooms
        .env
        .store()
        .append_event(&rimz::EventEnvelope::agent_launched(
            workspace.workspace_id,
            &workspace.session_name,
            &AgentKind::new_unchecked("claude"),
            rimz::store::event::AgentLaunchPayload {
                agent_id: rimz::ids::AgentSessionId::from("gone-session"),
                launch_id: Some(rimz::ids::AgentSessionId::from("gone-launch")),
                agent_name: "gone".to_owned(),
                agent_name_explicit: true,
                launch: rimz::agents::LaunchParams::default(),
                state: rimz::store::event::AgentLaunchState::Bound,
                run_id: None,
                pane_id: Some(gone.clone()),
                runtime_owner: None,
                worktree_path: Some(rooms.env.project_root.display().to_string()),
                worktree_branch: None,
                prompt: None,
                description: None,
            },
        ))
        .expect("seed agent bound to a closed pane");

    let send = rooms.pane(&["send", "--enter", "@gone", "echo GONE_MARK"]);
    let stderr = String::from_utf8_lossy(&send.stderr);
    assert_eq!(send.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains(&rooms.refusal(&gone)), "{stderr}");
}
