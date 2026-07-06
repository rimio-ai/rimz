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
fn split_pane_spells_requested_direction() {
    use crate::mux::{MuxBackend, SplitDirection, SplitPaneOptions};

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/zellij.log"
exit 0
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);

    for direction in [SplitDirection::Right, SplitDirection::Down] {
        backend
            .split_pane(SplitPaneOptions {
                direction,
                focus: true,
                ..Default::default()
            })
            .expect("split_pane");
    }

    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read shim log");
    assert!(
        log.contains("action new-pane --direction right"),
        "right split must be explicit:\n{log}",
    );
    assert!(
        log.contains("action new-pane --direction down"),
        "down split must be explicit:\n{log}",
    );
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
        pristine_birth: false,
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
fn reconcile_relists_before_tab_targeted_writes_from_topology_cache() {
    use crate::config::MultiplexerConfig;
    use crate::ids::WorkspaceId;
    use crate::ledger::paths::RuntimePaths;
    use crate::mux::zellij::pane_topology::{PaneTopologyCache, PaneTopologyPane};
    use crate::mux::{SidebarLiveness, SidebarPaneOptions, SidebarWidth};
    use crate::sidebar::cache::write_pane_topology_cache;
    use crate::sidebar::timing::unix_now_ms;

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
log="$dir/zellij.log"
state="$dir/sidebar-added"
printf '%s\n' "$*" >> "$log"

if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

case " $* " in
  *" action dump-layout "*)
    exit 0
    ;;
  *" action list-clients "*)
    printf '%s\n' 'CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND'
    printf '%s\n' '1 terminal_7 zsh'
    exit 0
    ;;
  *" action list-panes "*)
    if [ -f "$state" ]; then
      printf '%s\n' '[{"id":7,"is_plugin":false,"tab_id":42,"tab_position":1,"title":"zsh","pane_x":30,"pane_columns":90},{"id":8,"is_plugin":false,"tab_id":42,"tab_position":1,"title":"rimz-sidebar","pane_x":0,"pane_columns":30}]'
    else
      printf '%s\n' '[{"id":7,"is_plugin":false,"tab_id":42,"tab_position":1,"title":"zsh","pane_x":0,"pane_columns":120}]'
    fi
    exit 0
    ;;
  *" action new-pane "*)
    printf '%s\n' "mounted" > "$state"
    printf '%s\n' 'terminal_8'
    exit 0
    ;;
  *" action focus-pane-id "*|*" action move-pane "*|*" action resize "*)
    exit 0
    ;;
esac

exit 0
"#,
    );
    let runtime_root = tempfile::TempDir::new().expect("runtime tempdir");
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    let workspace_id = WorkspaceId::from_project_root(&project_root);
    let runtime = RuntimePaths::under(workspace_id.clone(), runtime_root.path()).expect("runtime");
    runtime.ensure_dirs().expect("runtime dirs");
    write_pane_topology_cache(
        &runtime,
        &PaneTopologyCache {
            session_name: "rimz-test".to_owned(),
            produced_at_ms: unix_now_ms(),
            focused_pane: Some(7),
            panes: vec![PaneTopologyPane {
                id: 7,
                is_plugin: false,
                is_held: false,
                exited: false,
                is_suppressed: false,
                is_floating: false,
                is_focused: true,
                tab_position: 1,
                tab_name: Some("work".to_owned()),
                pane_columns: Some(120),
                pane_x: Some(0),
                title: Some("zsh".to_owned()),
                pane_command: Some("zsh".to_owned()),
                terminal_command: Some("zsh".to_owned()),
            }],
        },
    )
    .expect("write topology cache");

    let opts = SidebarPaneOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id,
        project_root: project_root.clone(),
        cwd: project_root,
        birth_size: SidebarWidth::default().birth_size(Some(120)),
        rimz_bin: std::path::PathBuf::from("rimz"),
        replace_existing: false,
        pristine_birth: false,
        config: MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };

    let backend = ZellijBackend::with_program_and_runtime_for_test(&shim, runtime_root.path());
    let report = backend
        .reconcile_sidebars(&opts, &SidebarLiveness::default())
        .expect("reconcile_sidebars");

    assert_eq!(report.recovered, 1, "missing sidebar is added");
    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read shim log");
    let new_panes: Vec<&str> = log
        .lines()
        .filter(|line| line.contains(" action new-pane "))
        .collect();
    assert_eq!(new_panes.len(), 1, "one add issued:\n{log}");
    assert!(
        new_panes[0].contains("--tab-id 42"),
        "add must use CLI internal tab id after cache-triggered re-list:\n{log}",
    );
    assert!(
        !new_panes[0].contains("--tab-id 1"),
        "cache tab position must not target writes:\n{log}",
    );
}

#[cfg(unix)]
#[test]
fn commands_surface_session_not_found_banner() {
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
        .list_panes_bounded(Some("missing-room"), Duration::from_secs(2))
        .expect_err("banner should classify as session-not-found");
    assert!(
        matches!(err, MuxErr::SessionNotFound { ref session } if session == "missing-room"),
        "got: {err}",
    );

    let err = backend
        .tab_names("missing-room")
        .expect_err("banner should classify as session-not-found");

    assert!(
        matches!(err, MuxErr::SessionNotFound { ref session } if session == "missing-room"),
        "got: {err}",
    );
}

#[cfg(unix)]
#[test]
fn commands_surface_session_not_found_banner_nonzero_exit() {
    use std::time::Duration;

    use crate::mux::MuxErr;

    let (_temp, shim) = zellij_shim(
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

printf "Session 'missing-room' not found. The following sessions are active:\n" >&2
printf '\033[32;1mrimz-other\033[m [Created 6m ago]\n' >&2
exit 1
"#,
    );
    let backend = ZellijBackend::with_program_for_test(&shim);

    let err = backend
        .list_panes_bounded(Some("missing-room"), Duration::from_secs(2))
        .expect_err("nonzero banner should classify as session-not-found");
    assert!(
        matches!(err, MuxErr::SessionNotFound { ref session } if session == "missing-room"),
        "got: {err}",
    );
    assert!(
        !err.to_string().contains("rimz-other"),
        "typed error must not leak active session names: {err}",
    );

    let err = backend
        .tab_names("missing-room")
        .expect_err("nonzero banner should classify as session-not-found");
    assert!(
        matches!(err, MuxErr::SessionNotFound { ref session } if session == "missing-room"),
        "got: {err}",
    );
    assert!(
        !err.to_string().contains("rimz-other"),
        "typed error must not leak active session names: {err}",
    );
}

#[cfg(unix)]
#[test]
fn new_tab_confirmation_waits_for_layout_panes() {
    use crate::config::MultiplexerConfig;
    use crate::ids::WorkspaceId;
    use crate::mux::{
        LayoutColumn, LayoutPanes, PaneCmd, SidebarPaneOptions, SidebarWidth, TabOptions,
    };

    let (temp, shim) = zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
log="$dir/zellij.log"
tab="$dir/tab-created"
layout_ref="$dir/layout-path"
list_count="$dir/list-tabs-count"
printf '%s\n' "$*" >> "$log"

if [ "$1" = "--version" ]; then
  printf 'zellij 0.44.3\n'
  exit 0
fi

case " $* " in
  *" action dump-layout "*)
    printf 'layout {\n}\n'
    exit 0
    ;;
  *" action query-tab-names "*)
    printf 'main\n'
    if [ -f "$tab" ]; then
      printf 'work\n'
    fi
    exit 0
    ;;
  *" action new-tab "*)
    while [ "$#" -gt 0 ]; do
      if [ "$1" = "--layout" ]; then
        shift
        printf '%s' "$1" > "$layout_ref"
      fi
      shift
    done
    : > "$tab"
    exit 0
    ;;
  *" action list-tabs "*)
    count=$(cat "$list_count" 2>/dev/null || printf '0')
    count=$((count + 1))
    printf '%s\n' "$count" > "$list_count"
    printf '[{"name":"main","selectable_tiled_panes_count":1}'
    if [ -f "$tab" ]; then
      panes=0
      layout=$(cat "$layout_ref" 2>/dev/null || true)
      if [ "$count" -ge 3 ]; then
        if [ -n "$layout" ] && [ -f "$layout" ]; then
          panes=2
        else
          printf '%s\n' 'layout-missing-before-materialized' >> "$log"
        fi
      fi
      printf ',{"name":"work","selectable_tiled_panes_count":%s}' "$panes"
    fi
    printf ']\n'
    exit 0
    ;;
esac
"#,
    );
    let project_root = temp.path().join("project");
    std::fs::create_dir_all(&project_root).expect("mkdir project");
    let sidebar = SidebarPaneOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: WorkspaceId::from_project_root(&project_root),
        project_root: project_root.clone(),
        cwd: project_root.clone(),
        birth_size: SidebarWidth::default().birth_size(Some(120)),
        rimz_bin: std::path::PathBuf::from("rimz"),
        replace_existing: false,
        pristine_birth: false,
        config: MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };

    let backend = ZellijBackend::with_program_for_test(&shim);
    backend
        .open_tab(&TabOptions {
            session_name: "rimz-test".to_owned(),
            title: "work".to_owned(),
            cwd: project_root,
            panes: LayoutPanes {
                columns: vec![LayoutColumn {
                    panes: vec![PaneCmd {
                        argv: vec!["sleep".to_owned(), "600".to_owned()],
                    }],
                    stacked: false,
                }],
            },
            focus: true,
            dock_sidebar: true,
            sidebar,
        })
        .expect("open tab");

    let log = std::fs::read_to_string(temp.path().join("zellij.log")).expect("read shim log");
    let materialize_polls = log
        .lines()
        .filter(|line| line.contains("action list-tabs --json --panes"))
        .count();
    assert!(
        materialize_polls >= 3,
        "new-tab confirmation must wait for pane materialization, got log:\n{log}",
    );
    assert!(
        !log.contains("layout-missing-before-materialized"),
        "the temp layout file must stay alive until panes materialize:\n{log}",
    );
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("action new-tab "))
            .count(),
        1,
        "materialization polling should not create duplicate tabs:\n{log}",
    );
}

#[test]
fn runtime_dir_pins_full_zellij_env_surface() {
    let runtime = tempfile::TempDir::new().expect("runtime tempdir");
    let runtime = runtime.path().to_string_lossy().into_owned();
    let pinned = ZellijBackend::with_runtime_dir(&runtime).cmd();

    for key in [
        "XDG_RUNTIME_DIR",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "HOME",
        "TMPDIR",
    ] {
        assert_eq!(
            pinned.env.get(key),
            Some(&runtime),
            "{key} must point at the test runtime dir",
        );
    }

    let default = ZellijBackend::default().cmd();
    for key in [
        "XDG_RUNTIME_DIR",
        "XDG_STATE_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CACHE_HOME",
        "HOME",
        "TMPDIR",
    ] {
        assert!(
            !default.env.contains_key(key),
            "production backend must not override {key}",
        );
    }
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
    assert_eq!(MIN_STACKED_RESIZE_VERSION, (0, 42, 0));
    assert!(MIN_STACKED_RESIZE_VERSION >= MIN_ZELLIJ_VERSION);
}

#[test]
fn log_classifier_matches_leading_levels_and_panics_only() {
    use crate::mux::logtail::LogSeverity;

    assert_eq!(
        classify_log_line("ERROR failed to decode"),
        Some(LogSeverity::Error)
    );
    assert_eq!(
        classify_log_line("WARN slow client"),
        Some(LogSeverity::Warn)
    );
    assert_eq!(
        classify_log_line("Panic occured: over 1000 consecutive unknown messages"),
        Some(LogSeverity::Panic)
    );
    assert_eq!(
        classify_log_line("INFO later WARN text is not a level"),
        None
    );
    assert_eq!(classify_log_line("WARNING is not WARN token"), None);
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
    assert!(
        args.windows(2)
            .any(|pair| pair[0] == "--stacked-resize" && pair[1] == "true"),
        "Zellij 0.42 supports stacked_resize"
    );

    let args = zellij_options_args(&mouse_config, Some((0, 41, 9)));
    assert!(
        !args.iter().any(|arg| arg == "--stacked-resize"),
        "Zellij before 0.42 rejects stacked_resize"
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
    assert!(has("--auto-layout", "false"));
    assert!(has("--stacked-resize", "true"));
    assert!(has("--session-serialization", "false"));
    assert!(has("--disable-session-metadata", "true"));
    assert!(
        !args.iter().any(|arg| arg == "--web-sharing"),
        "normal rooms defer web sharing to the user's Zellij config"
    );
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
    assert!(has_unknown("--auto-layout", "false"));
    assert!(has_unknown("--session-serialization", "false"));
    assert!(has_unknown("--disable-session-metadata", "true"));
    assert!(!unknown.iter().any(|arg| arg == "--stacked-resize"));
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
    assert!(has("--auto-layout", "false"));
    assert!(has("--stacked-resize", "true"));
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
