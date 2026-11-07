//! Palette entry guards without a terminal or a live multiplexer.

use crate::common::Env;

#[test]
fn palette_without_a_terminal_names_the_popup_escape() {
    let env = Env::new();
    let output = env.rimz().arg("palette").output().unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("rimz palette needs a terminal; run `rimz palette --popup`"),
        "{stderr}"
    );
}

#[test]
fn palette_resolves_the_named_room_before_opening_a_popup() {
    let env = Env::new();
    let output = env
        .rimz()
        .args([
            "palette",
            "--popup",
            "--session-name",
            "missing-room",
            "--mux",
            "tmux",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "no room for session missing-room: run `rimz start` there, or pass --root <room>"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("multiplexer command"), "{stderr}");
}
