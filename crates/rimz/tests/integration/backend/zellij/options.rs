use std::time::Duration;

use rimz::mux::{MuxBackend, PresencePluginOptions, zellij};

use super::support::*;

const FRAME_TITLE: &str = "RIMZ-OPTION-FRAME";
const CONTENT_MARKER: &str = "RIMZ-OPTION-CONTENT";

#[test]
fn pane_frames_birth_overrides_config_in_both_directions() {
    require_zellij!();
    for frames in [true, false] {
        check_frames(!frames, Some(frames), false);
    }
}

#[test]
fn pane_frames_reconverge_live_without_rebirth() {
    require_zellij!();
    check_frames(false, None, true);
}

fn check_frames(baseline: bool, frames: Option<bool>, reapply: bool) {
    let room = LiveZellijSession::new("options");
    std::fs::write(
        room.path().join(".config/zellij/config.kdl"),
        format!("show_startup_tips false\nshow_release_notes false\npane_frames {baseline}\n"),
    )
    .expect("write baseline config");
    std::fs::write(room.path().join(".zshrc"), "").expect("disable first-run menu");
    let (_stub_dir, stub) = sidebar_stub_alive_for(600);
    let mut opts = sidebar_opts(room.name(), room.path(), stub, 120);
    // Birth forks the session server from this spawn, and Zellij resolves its
    // config dir from the server's own environment. Without the pin the server
    // reads the developer's real `~/.config/zellij` and the baseline above is
    // inert, which hides whether the room option overrode anything at all.
    opts.extra_env.insert(
        "HOME".to_owned(),
        room.path().to_string_lossy().into_owned(),
    );
    opts.extra_env.insert(
        "ZELLIJ_CONFIG_DIR".to_owned(),
        room.path()
            .join(".config/zellij")
            .to_string_lossy()
            .into_owned(),
    );
    opts.config.zellij.pane_frames = frames;
    publish_room_bin(room.path(), &opts);
    room.backend()
        .open_sidebar(&opts, None)
        .expect("birth room");
    let panes = wait_for_pane_count(room.path(), room.name(), 2);
    let work = panes
        .iter()
        .find(|pane| pane.spawn_command.is_none())
        .expect("birth work shell")
        .pane_id
        .clone();
    let mut client = AttachedClient::attach(&room, 120, 40);
    // A client attaching to a detached birth can land on the sidebar; where it
    // lands is not this test's subject, so steer it to the work pane.
    client.press_alt_until('l', &work, "birth work pane");
    // Zellij draws no frame around a pane that is the only frame-eligible one in
    // its tab, and the birth work pane's siblings are the borderless sidebar and
    // compact bar. `pane_frames` stays invisible until a second work pane opens,
    // so split first and hand focus back to the pane this test renames.
    for action in [
        &["new-pane", "--direction", "right"][..],
        &["focus-previous-pane"],
    ] {
        let output = room.action(action);
        assert!(output.status.success(), "{action:?}: {output:?}");
    }
    client.wait_until_focused(&work, "birth work pane after split");
    let renamed = room.action(&["rename-pane", FRAME_TITLE]);
    assert!(renamed.status.success(), "{renamed:?}");
    room.backend()
        .send_keys(&work, None, &format!("printf '{CONTENT_MARKER}\\n'\n"))
        .expect("print content marker");
    poll_until(
        Duration::from_secs(10),
        || Ok::<_, String>(rendered_screen(&client)),
        |output| output.contains(CONTENT_MARKER),
        "attached client rendered work content",
    );
    if frames.unwrap_or(baseline) {
        wait_for_frame(&client);
    } else {
        // The content marker proves the client rendered, rather than treating an
        // empty/unattached PTY as evidence that frames are disabled.
        std::thread::sleep(Duration::from_secs(1));
        let screen = rendered_screen(&client);
        assert!(
            !screen
                .lines()
                .next()
                .unwrap_or_default()
                .contains(FRAME_TITLE),
            "unexpected frame: {screen}",
        );
    }
    if reapply {
        opts.config.zellij.pane_frames = Some(true);
        let state =
            rimz::StatePaths::under(opts.workspace_id.clone(), room.path()).expect("room state");
        let sidebar = rimz::config::MachineConfig::load_lenient().sidebar.clone();
        room.backend()
            .converge_presence_plugin_for(&PresencePluginOptions {
                session_name: room.name().to_owned(),
                workspace_id: opts.workspace_id.clone(),
                wasm: zellij::ensure_presence_plugin_artifact().expect("embedded presence wasm"),
                rimz_bin: state.room_bin,
                focus_key: Some(sidebar.focus_key),
                zoom_key: Some(sidebar.zoom_key),
                session_options: zellij::zellij_session_options(&opts.config.zellij),
            })
            .expect("reconverge changed session options");
        wait_for_frame(&client);
        client.wait_until_focused(&work, "same birth work pane after reconfigure");
    }
    eprintln!(
        "live Zellij frames verified: baseline={baseline}, birth={frames:?}, reapply={reapply}"
    );
}

fn wait_for_frame(client: &AttachedClient) {
    poll_until(
        Duration::from_secs(15),
        || Ok::<_, String>(rendered_screen(client)),
        |output| {
            output
                .lines()
                .next()
                .unwrap_or_default()
                .contains(FRAME_TITLE)
        },
        "work pane frame title in attached client's output",
    );
}

fn rendered_screen(client: &AttachedClient) -> String {
    // A pane rename also appears in OSC window-title sequences with frames off.
    // Inspect displayed cells, not those non-rendering title notifications.
    // Frame assertions use the top row: the bottom status bar can name the pane too.
    let mut parser = vt100::Parser::new(40, 120, 0);
    parser.process(&client.output_bytes());
    parser.screen().contents()
}
