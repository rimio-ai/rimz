use super::*;

#[test]
fn version_parser_and_floor_hold() {
    assert_eq!(parse_version("zellij 0.41.2"), Some((0, 41, 2)));
    assert_eq!(parse_version("  zellij 1.2.3  \n"), Some((1, 2, 3)));
    assert_eq!(parse_version("zellij 0.44"), Some((0, 44, 0)));
    assert_eq!(parse_version("garbage"), None);

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
fn option_flags_gate_by_version() {
    assert!(mouse_click_through_args(true, None).is_empty());
    assert!(mouse_click_through_args(true, Some((0, 43, 9))).is_empty());
    assert!(mouse_click_through_args(false, Some((0, 44, 3))).is_empty());
    let expected = vec!["--mouse-click-through".to_owned(), "true".to_owned()];
    assert_eq!(mouse_click_through_args(true, Some((0, 44, 0))), expected);

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
fn zellij_options_render_defaults_and_unknown_version_floor() {
    let args = zellij_options_args(&ZellijConfig::default(), Some((0, 44, 3)));
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(
        !args.iter().any(|arg| arg == "--mouse-mode"),
        "`--mouse-mode true` disables mouse reporting on Zellij 0.44.3; rely on the default"
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

    let unknown = zellij_options_args(&ZellijConfig::default(), None);
    let has_unknown = |flag: &str, value: &str| {
        unknown
            .windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has_unknown("--session-serialization", "false"));
    assert!(!unknown.iter().any(|arg| arg == "--mouse-click-through"));
    assert!(!unknown.iter().any(|arg| arg == "--advanced-mouse-actions"));
    assert!(!unknown.iter().any(|arg| arg == "--mouse-hover-effects"));
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
