use super::*;

#[cfg(unix)]
fn zellij_shim(script: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::TempDir::new().expect("tempdir");
    let shim = temp.path().join("zellij");
    let mut file = std::fs::File::create(&shim).expect("create shim");
    file.write_all(script.as_bytes()).expect("write shim");
    let mut perms = file.metadata().expect("shim metadata").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&shim, perms).expect("chmod shim");
    drop(file);
    (temp, shim)
}

#[cfg(unix)]
#[test]
fn add_sidebar_timeout_never_closes_stdout_only_hint() {
    use crate::config::MultiplexerConfig;
    use crate::ids::WorkspaceId;
    use crate::mux::{SidebarPaneOptions, SidebarWidth};

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
log="$dir/zellij.log"
state="$dir/new-pane-count"
printf '%s\n' "$*" >> "$log"

if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

case " $* " in
  *" action list-panes "*)
    count=$(cat "$state" 2>/dev/null || printf '0')
    if [ "$count" -ge 2 ]; then
      printf '%s\n' '[{"id":7,"is_plugin":false,"tab_id":1,"title":"zsh","pane_x":30,"pane_columns":90},{"id":8,"is_plugin":false,"tab_id":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":30}]'
    else
      printf '%s\n' '[{"id":7,"is_plugin":false,"tab_id":1,"title":"zsh","pane_x":0,"pane_columns":120}]'
    fi
    exit 0
    ;;
  *" action new-pane "*)
    count=$(cat "$state" 2>/dev/null || printf '0')
    count=$((count + 1))
    printf '%s\n' "$count" > "$state"
    if [ "$count" -eq 1 ]; then
      printf 'terminal_7\n'
    else
      printf 'terminal_8\n'
    fi
    exit 0
    ;;
esac
"#,
    );
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    let log = temp.path().join("zellij.log");

    let width = SidebarWidth::default();
    let opts = SidebarPaneOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: WorkspaceId::from_project_root(&project_root),
        project_root: project_root.clone(),
        cwd: project_root,
        birth_size: width.birth_size(Some(120)),
        rimz_bin: std::path::PathBuf::from("rimz"),
        replace_existing: false,
        config: MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };

    let backend = ZellijBackend::with_program_for_test(&shim);
    assert_eq!(
        backend.add_sidebar_to_tab(&opts, 1).expect("add sidebar"),
        super::sidebar::DockOutcome::Docked,
    );
    let log = std::fs::read_to_string(log).expect("read shim log");
    assert!(
        !log.contains("close-pane --pane-id terminal_7"),
        "stdout-only hint for a pre-existing work pane must not be closed:\n{log}",
    );
    assert!(
        log.contains(
            "action new-pane --direction right --tab-id 1 --name rimz-sidebar --borderless true"
        ),
        "repair-created sidebar panes must be explicitly borderless:\n{log}",
    );
}

#[cfg(unix)]
#[test]
fn list_panes_surfaces_session_not_found_banner() {
    use std::time::Duration;

    use crate::mux::MuxErr;

    let (_temp, shim) = zellij_shim(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

printf '\033[32;1mrimz-other\033[m [Created 6m ago]\n'
printf "Session 'missing-room' not found. The following sessions are active:\n" >&2
exit 0
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);

    let err = backend
        .list_panes_bounded(Some("missing-room"), Duration::from_millis(200))
        .expect_err("banner should classify as session-not-found");

    assert!(
        matches!(err, MuxErr::SessionNotFound { ref session } if session == "missing-room"),
        "got: {err}",
    );
}

#[cfg(unix)]
#[test]
fn tab_names_surfaces_session_not_found_banner() {
    use crate::mux::MuxErr;

    let (_temp, shim) = zellij_shim(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

printf '\033[32;1mrimz-other\033[m [Created 6m ago]\n'
printf "Session 'missing-room' not found. The following sessions are active:\n" >&2
exit 0
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);

    let err = backend
        .tab_names("missing-room")
        .expect_err("banner should classify as session-not-found");

    assert!(
        matches!(err, MuxErr::SessionNotFound { ref session } if session == "missing-room"),
        "got: {err}",
    );
}

#[test]
fn version_parser_and_floor_hold() {
    assert_eq!(parse_version("zellij 0.41.2"), Some((0, 41, 2)));
    assert_eq!(parse_version("  zellij 1.2.3  \n"), Some((1, 2, 3)));
    assert_eq!(parse_version("zellij 0.44"), Some((0, 44, 0)));
    assert_eq!(parse_version("garbage"), None);

    assert!((0, 41, 0) >= MIN_ZELLIJ_VERSION);
    assert!((0, 44, 3) >= MIN_ZELLIJ_VERSION);
    assert!((0, 40, 9) < MIN_ZELLIJ_VERSION);
    assert_eq!(STACK_PANES_MIN_ZELLIJ, (0, 42, 0));
    assert!(STACK_PANES_MIN_ZELLIJ >= MIN_ZELLIJ_VERSION);
    assert!((0, 42, 0) >= STACK_PANES_MIN_ZELLIJ);
    assert!((0, 44, 3) >= STACK_PANES_MIN_ZELLIJ);
    assert!((0, 41, 9) < STACK_PANES_MIN_ZELLIJ);
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

    let mouse_config = ZellijConfig {
        advanced_mouse_actions: Some(true),
        mouse_hover_effects: Some(false),
        ..ZellijConfig::default()
    };
    let args = zellij_options_args(&mouse_config, Some((0, 42, 9)));
    assert!(
        !args.iter().any(|arg| arg == "--advanced-mouse-actions"),
        "Zellij before 0.43 rejects advanced mouse action options"
    );
    assert!(
        !args.iter().any(|arg| arg == "--mouse-hover-effects"),
        "Zellij before 0.44 rejects mouse hover effect options"
    );

    let args = zellij_options_args(&mouse_config, Some((0, 43, 0)));
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has("--advanced-mouse-actions", "true"));
    assert!(!args.iter().any(|arg| arg == "--mouse-hover-effects"));

    let args = zellij_options_args(&mouse_config, Some((0, 44, 0)));
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has("--advanced-mouse-actions", "true"));
    assert!(has("--mouse-hover-effects", "false"));
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
    assert!(has("--focus-follows-mouse", "false"));
    assert!(has("--auto-layout", "true"));
    assert!(has("--session-serialization", "false"));
    for flag in [
        "--advanced-mouse-actions",
        "--mouse-hover-effects",
        "--pane-frames",
        "--copy-clipboard",
        "--support-kitty-keyboard-protocol",
        "--osc8-hyperlinks",
    ] {
        assert!(
            !args.iter().any(|arg| arg == flag),
            "unset optional {flag} must defer to Zellij config: {args:?}",
        );
    }

    let unknown = zellij_options_args(&ZellijConfig::default(), None);
    let has_unknown = |flag: &str, value: &str| {
        unknown
            .windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has_unknown("--auto-layout", "true"));
    assert!(has_unknown("--session-serialization", "false"));
    assert!(!unknown.iter().any(|arg| arg == "--mouse-click-through"));
    assert!(!unknown.iter().any(|arg| arg == "--advanced-mouse-actions"));
    assert!(!unknown.iter().any(|arg| arg == "--mouse-hover-effects"));
}

#[test]
fn zellij_options_render_configured_optionals() {
    let config = ZellijConfig {
        mouse_mode: Some(false),
        pane_frames: Some(true),
        on_force_close: Some(crate::config::ZellijForceClose::Quit),
        scroll_buffer_size: Some(200_000),
        show_startup_tips: Some(true),
        show_release_notes: Some(true),
        copy_clipboard: Some(crate::config::ZellijClipboard::Primary),
        copy_on_select: Some(false),
        support_kitty_keyboard_protocol: Some(false),
        osc8_hyperlinks: Some(false),
        ..ZellijConfig::default()
    };
    let args = zellij_options_args(&config, Some((0, 44, 3)));
    let has = |flag: &str, value: &str| {
        args.windows(2)
            .any(|pair| pair[0] == flag && pair[1] == value)
    };
    assert!(has("--mouse-mode", "false"));
    assert!(has("--pane-frames", "true"));
    assert!(has("--on-force-close", "quit"));
    assert!(has("--scroll-buffer-size", "200000"));
    assert!(has("--show-startup-tips", "true"));
    assert!(has("--show-release-notes", "true"));
    assert!(has("--copy-clipboard", "primary"));
    assert!(has("--copy-on-select", "false"));
    assert!(has("--support-kitty-keyboard-protocol", "false"));
    assert!(has("--osc8-hyperlinks", "false"));
}

#[test]
fn zellij_options_render_auto_layout_opt_out() {
    let config = ZellijConfig {
        auto_layout: false,
        ..ZellijConfig::default()
    };
    let args = zellij_options_args(&config, Some((0, 44, 3)));
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--auto-layout" && pair[1] == "false")
    );
}
