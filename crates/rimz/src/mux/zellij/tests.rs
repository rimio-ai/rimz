use super::*;

#[test]
fn version_parser_accepts_three_dot_form() {
    assert_eq!(parse_version("zellij 0.41.2"), Some((0, 41, 2)));
    assert_eq!(parse_version("  zellij 1.2.3  \n"), Some((1, 2, 3)));
    assert_eq!(parse_version("zellij 0.44"), Some((0, 44, 0)));
    assert_eq!(parse_version("garbage"), None);
}
#[test]
fn min_version_threshold_holds() {
    assert!((0, 41, 0) >= MIN_ZELLIJ_VERSION);
    assert!((0, 44, 3) >= MIN_ZELLIJ_VERSION);
    assert!((0, 40, 9) < MIN_ZELLIJ_VERSION);
}
#[test]
fn version_serves_the_memoized_probe() {
    let backend = ZellijBackend::default();
    backend
        .version
        .set("zellij 9.9.9".to_owned())
        .expect("a fresh instance has not probed yet");
    // The cache is consulted before any probe: the seeded value comes back
    // verbatim — no `zellij --version` fork, no overwrite by a real binary.
    assert_eq!(backend.version().expect("cached version"), "zellij 9.9.9");
}
#[test]
fn mouse_click_through_args_gate_on_version() {
    // Older or unknown Zellij does not know the flag — omit it.
    assert!(mouse_click_through_args(true, None).is_empty());
    assert!(mouse_click_through_args(true, Some((0, 43, 9))).is_empty());
    assert!(mouse_click_through_args(true, Some((0, 41, 0))).is_empty());
    assert!(mouse_click_through_args(false, Some((0, 44, 3))).is_empty());
    // The release that added the option, and newer, carry it.
    let expected = vec!["--mouse-click-through".to_owned(), "true".to_owned()];
    assert_eq!(mouse_click_through_args(true, Some((0, 44, 0))), expected);
    assert_eq!(mouse_click_through_args(true, Some((0, 44, 3))), expected);
}
#[test]
fn zellij_options_render_room_defaults() {
    let args = zellij_options_args(&ZellijConfig::default(), Some((0, 44, 3)));
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(
        !args.iter().any(|arg| arg == "--mouse-mode"),
        "`--mouse-mode true` disables mouse reporting on Zellij 0.44.3; \
             rely on Zellij's default enabled state"
    );
    assert!(has("--default-mode", "locked"));
    assert!(has("--mouse-click-through", "true"));
    assert!(has("--advanced-mouse-actions", "false"));
    assert!(has("--mouse-hover-effects", "false"));
    assert!(has("--focus-follows-mouse", "false"));
    assert!(has("--pane-frames", "false"));
    assert!(has("--copy-clipboard", "system"));
    assert!(has("--support-kitty-keyboard-protocol", "true"));
    assert!(has("--session-serialization", "false"));
}
#[test]
fn zellij_options_gate_newer_mouse_flags() {
    let args = zellij_options_args(&ZellijConfig::default(), Some((0, 42, 9)));
    assert!(
        !args.iter().any(|arg| arg == "--advanced-mouse-actions"),
        "Zellij before 0.43 rejects advanced mouse action options"
    );
    assert!(
        !args.iter().any(|arg| arg == "--mouse-hover-effects"),
        "Zellij before 0.44 rejects mouse hover effect options"
    );

    let args = zellij_options_args(&ZellijConfig::default(), Some((0, 43, 0)));
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has("--advanced-mouse-actions", "false"));
    assert!(!args.iter().any(|arg| arg == "--mouse-hover-effects"));
}
#[test]
fn zellij_options_render_mouse_opt_out() {
    let config = ZellijConfig {
        mouse_mode: false,
        ..ZellijConfig::default()
    };
    let args = zellij_options_args(&config, Some((0, 44, 3)));
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--mouse-mode" && pair[1] == "false")
    );
}
#[test]
fn session_serialization_is_not_version_gated() {
    // Unlike `mouse-click-through`, the flag predates Rimz's Zellij floor, so
    // it must be present even when the version probe returns nothing.
    let args = zellij_options_args(&ZellijConfig::default(), None);
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has("--session-serialization", "false"));
    // And the gated option is correctly absent at an unknown version.
    assert!(!args.iter().any(|arg| arg == "--mouse-click-through"));
    assert!(!args.iter().any(|arg| arg == "--advanced-mouse-actions"));
    assert!(!args.iter().any(|arg| arg == "--mouse-hover-effects"));
}
