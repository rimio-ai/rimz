use super::*;

#[cfg(unix)]
#[test]
fn fit_views_isolates_read_and_fit_failures_in_input_order() {
    use crate::ids::ViewId;
    use crate::mux::{FittedPane, ViewFit, ViewSize};

    let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(
        "#!/bin/sh\ndir=$(dirname \"$0\")\nprintf '%s\\n' \"$*\" >> \"$dir/argv\"\nif [ -f \"$dir/read-stdin\" ]; then\ncat > \"$dir/fit-stdin\"\ncat \"$dir/fit\"\nelse\ncat > \"$dir/read-stdin\"\ncat \"$dir/read\"\nfi\nexit 1\n",
    );
    let mut before = String::new();
    for index in 0..6 {
        before.push_str(&format!("__rimz_script_begin_{index}__\n%1 1\n%2 0\n"));
        if index != 2 {
            before.push_str(&format!("__rimz_script_end_{index}__\n"));
        }
    }
    std::fs::write(temp.path().join("read"), before).unwrap();
    std::fs::write(
        temp.path().join("fit"),
        "__rimz_script_begin_0__\n%1 65\n%2 65\n__rimz_script_end_0__\n\
         __rimz_script_begin_1__\nskipped\n__rimz_script_end_1__\n\
         __rimz_script_begin_2__\n__rimz_script_end_2__\n\
         __rimz_script_begin_3__\n%3 65\n\
         __rimz_script_begin_4__\nbad rows\n__rimz_script_end_4__\n\
         __rimz_script_begin_5__\n__rimz_script_end_5__\n",
    )
    .unwrap();
    let mut backend = TmuxBackend::with_socket("/test/socket");
    backend.program = Some(shim);
    let views = ["@3", "@1", "@8", "@2", "@4", "@7"].map(ViewId::new_unchecked);
    let outcomes = backend
        .fit_views(
            &views,
            ViewSize {
                cols: 240,
                rows: 65,
            },
            std::num::NonZeroU16::new(60).unwrap(),
        )
        .unwrap();
    assert_eq!(
        outcomes,
        [
            ViewFit::Fitted(
                ["%1", "%2"]
                    .map(|pane| (
                        crate::ids::PaneId::from_parts(crate::ids::MuxName::Tmux, pane),
                        FittedPane {
                            rows: 65,
                            cols: None
                        },
                    ))
                    .into()
            ),
            ViewFit::Skipped,
            ViewFit::Failed,
            ViewFit::Failed,
            ViewFit::Failed,
            ViewFit::Failed,
        ],
        "a failed sidebar reassertion still supplies rows; other failures stay per-view"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("argv")).unwrap(),
        "-S /test/socket -u source-file -\n".repeat(2),
        "all reads and all fits each share one client invocation"
    );
    let script = std::fs::read_to_string(temp.path().join("fit-stdin")).unwrap();
    let lines: Vec<_> = script.lines().collect();
    for index in [0, 1, 3, 4, 5] {
        assert!(lines[index].contains(&format!("'-t' '{}'", views[index].as_str())));
    }
    assert!(!lines[2].contains("resize-window"));
}

#[cfg(unix)]
#[test]
fn fit_views_reports_transport_failure_instead_of_skipping() {
    let (_temp, backend) = script_shim("-:2: unknown command: broken\n", "");
    let result = backend.fit_views(
        &[crate::ids::ViewId::new_unchecked("@1")],
        crate::mux::ViewSize {
            cols: 240,
            rows: 65,
        },
        std::num::NonZeroU16::new(60).unwrap(),
    );
    assert!(
        result.is_err(),
        "a script that never ran must fail: {result:?}"
    );
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("unknown command: broken")
    );
}

#[cfg(unix)]
fn script_shim(stdout: &str, stderr: &str) -> (tempfile::TempDir, TmuxBackend) {
    let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(
        "#!/bin/sh\ndir=$(dirname \"$0\")\nprintf '%s\\n' \"$*\" >> \"$dir/argv\"\ncat > \"$dir/stdin\"\ncat \"$dir/stdout\"\ncat \"$dir/stderr\" >&2\nexit 1\n",
    );
    std::fs::write(temp.path().join("stdout"), stdout).unwrap();
    std::fs::write(temp.path().join("stderr"), stderr).unwrap();
    let mut backend = TmuxBackend::with_socket("/test/socket");
    backend.program = Some(shim);
    (temp, backend)
}

#[test]
fn script_output_keeps_failed_lines_separate_and_preserves_stderr() {
    let output = parse_script_output(
        "__rimz_script_begin_0__\nfirst\n__rimz_script_end_0__\n\
         __rimz_script_begin_1__\npartial\n\
         __rimz_script_begin_2__\nlast\n__rimz_script_end_2__\n",
        "no such window: @999\n",
        3,
    );
    assert!(output.is_ok(), "a runtime failure costs only its line");
    let output = output.unwrap();
    assert_eq!(
        output.lines,
        [
            ScriptLine {
                completed: true,
                stdout: "first\n".into()
            },
            ScriptLine {
                completed: false,
                stdout: "partial\n".into()
            },
            ScriptLine {
                completed: true,
                stdout: "last\n".into()
            },
        ],
    );
    assert_eq!(output.stderr, "no such window: @999\n");
}

#[test]
fn script_parse_error_reports_stdout_without_markers() {
    let error = parse_script_output("-:2: unknown command: not-a-command\n", "", 2)
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("-:2: unknown command: not-a-command"),
        "{error}"
    );
    let error = parse_script_output("", "connection refused", 2)
        .unwrap_err()
        .to_string();
    assert!(error.contains("connection refused"), "{error}");
}

#[cfg(unix)]
#[test]
fn script_transport_quotes_words_and_accepts_partial_execution() {
    let (temp, backend) = script_shim(
        "__rimz_script_begin_0__\nhello\n__rimz_script_end_0__\n",
        "a later line failed\n",
    );
    let output = backend.run_script(&[vec![
        vec![
            "display-message".into(),
            "-p".into(),
            "# lead ' ; #{pane_id}".into(),
        ],
        vec!["display-message".into(), "-p".into(), "done".into()],
    ]]);
    assert!(
        output.is_ok(),
        "markers, not the exit status, prove execution"
    );
    let output = output.unwrap();
    assert_eq!(output.lines[0].stdout, "hello\n");
    assert!(output.lines[0].completed);
    assert_eq!(output.stderr, "a later line failed\n");
    assert_eq!(
        std::fs::read_to_string(temp.path().join("argv")).unwrap(),
        "-S /test/socket -u source-file -\n",
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("stdin")).unwrap(),
        "'display-message' '-p' '__rimz_script_begin_0__' ; 'display-message' '-p' '# lead '\\'' ; #{pane_id}' ; 'display-message' '-p' 'done' ; 'display-message' '-p' '__rimz_script_end_0__'\n",
    );
}

#[cfg(unix)]
#[test]
fn script_transport_refuses_line_breaks_before_spawning() {
    let (temp, backend) = script_shim("", "");
    let errors = ["line\nbreak", "line\rbreak"].map(|word| {
        backend
            .run_script(&[vec![vec!["display-message".into(), word.into()]]])
            .unwrap_err()
            .to_string()
    });
    assert!(
        errors.iter().all(|error| error.contains("line break")),
        "{errors:?}"
    );
    assert!(!temp.path().join("argv").exists());
}

#[cfg(unix)]
#[test]
fn script_transport_empty_input_does_not_spawn() {
    let (temp, backend) = script_shim("", "");
    assert_eq!(backend.run_script(&[]).ok(), Some(ScriptOutput::default()));
    assert!(!temp.path().join("argv").exists());
}

#[cfg(unix)]
fn window_options_shim(
    windows: &str,
    read_stdout: &str,
    script_stdout: &str,
) -> (tempfile::TempDir, TmuxBackend) {
    let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(
        "#!/bin/sh\ndir=$(dirname \"$0\")\nprintf '%s\\n' \"$*\" >> \"$dir/argv\"\n\
         if [ \"$4\" = source-file ]; then\n\
         if [ -f \"$dir/read-stdin\" ]; then\n\
         cat > \"$dir/stdin\"\ncat \"$dir/script-stdout\"\n\
         else\ncat > \"$dir/read-stdin\"\ncat \"$dir/read-stdout\"\nfi\n\
         printf 'no such window: @999\\n' >&2\nexit 1\nfi\n\
         case \"$*\" in\n*'#{allow-passthrough}'*) cat \"$dir/windows\" ;;\n\
         *) awk -F '|' '/^@/ { print $1 \"|\" $2 \"|\" $4 \"|\" $7; next } { print }' \"$dir/windows\" ;;\nesac\n",
    );
    std::fs::write(temp.path().join("windows"), windows).unwrap();
    std::fs::write(temp.path().join("read-stdout"), read_stdout).unwrap();
    std::fs::write(temp.path().join("script-stdout"), script_stdout).unwrap();
    let mut backend = TmuxBackend::with_socket("/test/socket");
    backend.program = Some(shim);
    (temp, backend)
}

#[cfg(unix)]
#[test]
fn window_options_matching_effective_values_need_no_writes() {
    let (temp, backend) = window_options_shim(
        "bottom\nsingle\nuser|format\n@4||on|on|bottom|single||user|format\n",
        "__rimz_script_begin_0__\non\nbottom\nsingle\nuser|format\n__rimz_script_end_0__\n",
        "",
    );
    assert!(
        backend
            .apply_window_options("room", &TmuxConfig::default())
            .is_ok()
    );
    let argv = std::fs::read_to_string(temp.path().join("argv")).unwrap();
    assert_eq!(
        argv.lines().count(),
        2,
        "the fixed-size batch and window-scoped read need no write script: {argv}"
    );
    assert!(!temp.path().join("stdin").exists());
    let read = std::fs::read_to_string(temp.path().join("read-stdin")).unwrap();
    for key in [
        "allow-passthrough",
        "pane-border-status",
        "pane-border-lines",
        "pane-border-format",
    ] {
        assert!(
            read.contains(&format!("'show-options' '-wAv' '-t' '@4' '{key}'")),
            "{read}"
        );
    }
}

#[cfg(unix)]
#[test]
fn window_options_script_writes_only_the_differing_key() {
    let (temp, backend) = window_options_shim(
        "bottom\nsingle\nuser|format\n@4||on|off|bottom|single||user|format\n",
        "__rimz_script_begin_0__\non\nbottom\nsingle\nuser|format\n__rimz_script_end_0__\n",
        "__rimz_script_begin_0__\n__rimz_script_end_0__\n",
    );
    assert!(
        backend
            .apply_window_options("room", &TmuxConfig::default())
            .is_ok()
    );
    assert!(
        temp.path().join("stdin").exists(),
        "differing options travel on stdin"
    );
    let script = std::fs::read_to_string(temp.path().join("stdin")).unwrap();
    assert_eq!(script.lines().count(), 1);
    assert!(
        script.contains("'set-window-option' '-t' '@4' 'aggressive-resize' 'on'"),
        "{script}"
    );
    assert!(!script.contains("pane-border"), "{script}");
    assert!(!script.contains("allow-passthrough"), "{script}");
}

#[cfg(unix)]
#[test]
fn window_options_partial_failure_still_applies_other_windows() {
    let (temp, backend) = window_options_shim(
        "bottom\nsingle\nuser|format\n\
         @999||on|off|bottom|single||user|format\n\
         @998||on|off|bottom|single||user|format\n\
         @4||on|off|bottom|single||user|format\n",
        "__rimz_script_begin_0__\non\nbottom\nsingle\nuser|format\n__rimz_script_end_0__\n\
         __rimz_script_begin_1__\non\n\
         __rimz_script_begin_2__\non\nbottom\nsingle\nuser|format\n__rimz_script_end_2__\n",
        "__rimz_script_begin_0__\n__rimz_script_begin_1__\n__rimz_script_end_1__\n",
    );
    assert!(
        backend
            .apply_window_options("room", &TmuxConfig::default())
            .is_ok()
    );
    assert!(
        temp.path().join("stdin").exists(),
        "windows share a script, not an argv batch"
    );
    assert_eq!(
        std::fs::read_to_string(temp.path().join("argv"))
            .unwrap()
            .lines()
            .count(),
        3
    );
    let script = std::fs::read_to_string(temp.path().join("stdin")).unwrap();
    assert!(script.contains("'-t' '@999'"), "{script}");
    assert!(!script.contains("'-t' '@998'"), "{script}");
    assert!(script.contains("'-t' '@4'"), "{script}");
}

#[cfg(unix)]
#[test]
fn window_options_all_lines_failing_refuses_room_open() {
    let (_temp, backend) = window_options_shim(
        "bottom\nsingle\nuser|format\n@999||on|off|bottom|single||user|format\n",
        "__rimz_script_begin_0__\non\nbottom\nsingle\nuser|format\n__rimz_script_end_0__\n",
        "__rimz_script_begin_0__\n",
    );
    assert!(
        backend
            .apply_window_options("room", &TmuxConfig::default())
            .is_err(),
        "a listed session with no applied window must fail"
    );
}

#[cfg(unix)]
#[test]
fn window_options_failed_read_does_not_count_a_matching_window() {
    let (temp, backend) = window_options_shim(
        "bottom\nsingle\nuser|format\n@999||on|on|bottom|single||user|format\n",
        "__rimz_script_begin_0__\non\n",
        "",
    );
    assert!(
        backend
            .apply_window_options("room", &TmuxConfig::default())
            .is_err(),
        "a window that closed during the read was not applied"
    );
    assert!(!temp.path().join("stdin").exists());
}

#[cfg(unix)]
#[test]
fn window_options_read_transport_failure_refuses_room_open() {
    let (temp, backend) = window_options_shim(
        "bottom\nsingle\nuser|format\n@4||on|on|bottom|single||user|format\n",
        "-:2: unknown command: broken\n",
        "",
    );
    assert!(
        backend
            .apply_window_options("room", &TmuxConfig::default())
            .is_err(),
        "a read that never ran cannot prove the window matches"
    );
    assert!(!temp.path().join("stdin").exists());
}

#[cfg(unix)]
fn detached_geometry_shim(read: &str) -> (tempfile::TempDir, TmuxBackend) {
    let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(&format!(
        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$(dirname \"$0\")/argv\"\nprintf '%s' '{read}'\n",
    ));
    let mut backend = TmuxBackend::with_socket("/test/socket");
    backend.program = Some(shim);
    (temp, backend)
}

#[cfg(unix)]
#[test]
fn detached_geometry_matching_probe_needs_no_writes() {
    let (temp, backend) = detached_geometry_shim("212x50|on|@4|212|49|latest\n\n");
    backend.normalize_detached_geometry("room", 212, 50);
    let argv = std::fs::read_to_string(temp.path().join("argv")).unwrap();
    assert_eq!(
        argv.lines().count(),
        1,
        "unchanged geometry needs only the read: {argv}"
    );
}

#[cfg(unix)]
#[test]
fn detached_geometry_changes_only_current_window_and_restores_local_option() {
    let (temp, backend) = detached_geometry_shim("80x24|on|@4|80|24|smallest\nsmallest\n");
    backend.normalize_detached_geometry("room", 212, 50);
    let argv = std::fs::read_to_string(temp.path().join("argv")).unwrap();
    assert_eq!(argv.lines().count(), 2);
    assert_eq!(
        argv.lines().nth(1).unwrap(),
        "-S /test/socket -u set-option -t room default-size 212x50 ; resize-window -t @4 -x 212 -y 49 ; set-window-option -t @4 window-size smallest"
    );
}

#[cfg(unix)]
#[test]
fn detached_geometry_manual_window_is_not_resized() {
    let (temp, backend) = detached_geometry_shim("212x50|on|@4|80|24|manual\nmanual\n");
    backend.normalize_detached_geometry("room", 212, 50);
    let argv = std::fs::read_to_string(temp.path().join("argv")).unwrap();
    assert_eq!(
        argv.lines().count(),
        1,
        "manual geometry stays pinned: {argv}"
    );
}

#[cfg(unix)]
#[test]
fn detached_geometry_status_rows_and_inherited_option_are_preserved() {
    let logs = [("off", 50), ("2", 48), ("5", 45)].map(|(status, expected_rows)| {
        let (temp, backend) =
            detached_geometry_shim(&format!("212x50|{status}|@4|80|24|latest\n\n"));
        backend.normalize_detached_geometry("room", 212, 50);
        let argv = std::fs::read_to_string(temp.path().join("argv")).unwrap();
        (argv, expected_rows)
    });
    for (argv, expected_rows) in logs {
        assert!(
            argv.contains(&format!("resize-window -t @4 -x 212 -y {expected_rows}")),
            "{argv}"
        );
        assert!(
            argv.contains("set-window-option -u -t @4 window-size"),
            "{argv}"
        );
        assert!(!argv.contains("set-option -t room default-size"), "{argv}");
    }
}

#[cfg(unix)]
use crate::ids::MuxName;

#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
#[test]
fn pane_content_size_reads_dimensions_and_preserves_zero() {
    for (rows, cols) in [(38, 118), (0, 118)] {
        let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(&format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > \"$(dirname \"$0\")/argv\"\nprintf '{rows} {cols}\\n'\n"
        ));
        let mut backend = TmuxBackend::with_socket("/test/socket");
        backend.program = Some(shim);
        let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
        assert_eq!(
            backend
                .pane_content_size(&pane, None, Duration::from_secs(2))
                .ok(),
            Some(Some(crate::mux::PaneContentSize { rows, cols }))
        );
        assert_eq!(
            std::fs::read_to_string(temp.path().join("argv"))
                .unwrap()
                .trim(),
            "-S /test/socket -u display-message -p -t %7 #{pane_height} #{pane_width}"
        );
    }
}

#[cfg(unix)]
#[test]
fn pane_content_size_distinguishes_missing_pane_from_command_failure() {
    for (stderr, missing) in [("can't find pane: %7", true), ("server unavailable", false)] {
        let (_temp, shim) = crate::mux::zellij::tests::support::zellij_shim(&format!(
            "#!/bin/sh\nprintf '%s\\n' \"{stderr}\" >&2\nexit 1\n"
        ));
        let mut backend = TmuxBackend::with_socket("/test/socket");
        backend.program = Some(shim);
        let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
        let result = backend.pane_content_size(&pane, None, Duration::from_secs(2));
        if missing {
            assert_eq!(result.ok(), Some(None));
        } else {
            assert!(result.is_err());
        }
    }
}

#[cfg(unix)]
#[test]
fn command_failure_omits_socket_from_native_stderr() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let shim = temp.path().join("tmux");
    std::fs::write(&shim, "#!/bin/sh\nprintf 'error connecting to %s (No such file or directory)\\n' \"$2\" >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    let socket = temp.path().join("socket with spaces");
    let mut spec = TmuxBackend::with_socket(&socket)
        .cmd()
        .args(["capture-pane", "-p"]);
    spec.program = shim.to_string_lossy().into_owned();
    let error = spec.run().unwrap_err().to_string();
    assert!(!error.contains("socket with spaces"), "{error}");
    assert!(!error.contains("spaces capture-pane"), "{error}");
    assert!(error.contains("No such file or directory"), "{error}");
}

fn session_options() -> crate::mux::SessionOptions {
    crate::mux::SessionOptions {
        session_name: "rimz-test".to_owned(),
        workspace_id: crate::ids::WorkspaceId::from_project_root(Path::new("/project")),
        project_root: PathBuf::from("/project"),
        extra_env: Default::default(),
        cwd: PathBuf::from("/project"),
        config: Default::default(),
        detected_size: None,
        truecolor: false,
    }
}

/// Argv past the `-S <socket> -u` prefix every managed command carries, so a verb
/// assertion stays about the verb. [`managed_endpoint_prefixes_every_command`]
/// owns the prefix itself.
fn verb_args(spec: &CommandSpec) -> &[String] {
    assert_eq!(
        &spec.args[..1],
        ["-S"],
        "every tmux command must address an explicit socket",
    );
    assert_eq!(
        spec.args.get(2).map(String::as_str),
        Some("-u"),
        "every tmux command must preserve UTF-8 regardless of the caller's locale",
    );
    &spec.args[3..]
}

#[test]
fn managed_endpoint_prefixes_every_command() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let spec = backend.cmd();

    assert_eq!(
        spec.args,
        ["-S", "/run/user/1000/rimz/tmux/server", "-u"],
        "commands address the RimZ-owned server with a UTF-8 client",
    );
    // A server inherits its cwd from the client that births it. Since tmux 3.7,
    // `-c` needs the server's `getcwd()` to succeed, so a deleted cwd strands
    // panes. Earlier releases honour `-c` regardless.
    assert_eq!(spec.cwd.as_deref(), Some(Path::new("/")));
    assert!(
        spec.env_remove.contains("TMUX"),
        "an inherited $TMUX would let an ambient server capture a managed command",
    );
}

#[test]
fn new_session_birth_removes_outer_mux_context() {
    let opts = session_options();
    let spec =
        TmuxBackend::with_socket("/test/socket").new_session_command(&opts, &Default::default());
    for key in crate::mux::AMBIENT_MUX_ENV.into_iter().chain([
        "RIMZ_REMOTE_CLIENT_VERSION",
        "RIMZ_REMOTE_LINEAGE",
        "RIMZ_REMOTE_RECONNECT",
        "RIMZ_REMOTE_SUPERVISED",
        "RIMZ_REMOTE_FORCE_VERSION",
        "RIMZ_CLIENT_SIZE",
        "RIMZ_OUTER_SCROLL_BRACKET",
    ]) {
        assert!(
            spec.env_remove.contains(key),
            "{key} remains inherited at birth"
        );
    }
    assert_eq!(verb_args(&spec)[0], "new-session");
}

#[test]
fn existing_session_attach_targets_the_managed_server() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let spec = backend.attach_existing_command("rimz-test");

    assert_eq!(verb_args(&spec), ["attach", "-t", "rimz-test"]);
}

#[test]
fn global_mux_context_removal_marks_mux_and_remote_client_names() {
    let spec = TmuxBackend::with_socket("/test/socket").global_mux_context_removal_command();
    let commands = verb_args(&spec).split(|arg| arg == ";").collect::<Vec<_>>();
    let expected = [
        "TMUX",
        "TMUX_PANE",
        "ZELLIJ",
        "ZELLIJ_PANE_ID",
        "ZELLIJ_SESSION_NAME",
        "RIMZ_REMOTE_CLIENT_VERSION",
        "RIMZ_REMOTE_LINEAGE",
        "RIMZ_REMOTE_RECONNECT",
        "RIMZ_REMOTE_SUPERVISED",
        "RIMZ_REMOTE_FORCE_VERSION",
        "RIMZ_CLIENT_SIZE",
        "RIMZ_OUTER_SCROLL_BRACKET",
    ];
    assert_eq!(commands.len(), expected.len());
    for (command, key) in commands.into_iter().zip(expected) {
        assert_eq!(command, ["set-environment", "-g", "-r", key]);
    }
}

#[cfg(unix)]
#[test]
fn ensure_session_repairs_global_mux_context_before_and_after_birth() {
    for (pre_birth_error, duplicate) in [
        ("", false),
        ("", true),
        ("no server running on server", false),
        (
            "error connecting to server (No such file or directory)",
            false,
        ),
    ] {
        let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(&format!(
            r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$*" >> "$dir/argv"
if [ "$4" = set-environment ] && [ "$5" = -g ] && [ ! -e "$dir/repaired" ]; then
    touch "$dir/repaired"
    if [ -n '{pre_birth_error}' ]; then printf '%s\n' '{pre_birth_error}' >&2; exit 1; fi
fi
if [ "$4" = new-session ] && [ '{duplicate}' = true ]; then
    printf 'duplicate session: rimz-test\n' >&2
    exit 1
fi
"#
        ));
        let mut backend = TmuxBackend::with_socket(temp.path().join("server"));
        backend.program = Some(shim);
        let opts = session_options();
        backend.ensure_session(&opts).expect("ensure session");
        let log = std::fs::read_to_string(temp.path().join("argv")).unwrap();
        let commands = log.lines().collect::<Vec<_>>();
        assert!(
            commands[0].contains("set-environment -g -r TMUX"),
            "repair must precede new-session: {log}"
        );
        let birth = commands
            .iter()
            .position(|line| line.contains(" new-session "))
            .unwrap();
        let repair = backend.global_mux_context_removal_command().args.join(" ");
        assert_eq!(commands[0], repair);
        assert!(
            commands[birth + 1..].contains(&repair.as_str()),
            "post-birth repair: {log}"
        );
    }
}

#[cfg(unix)]
fn birth_cwd_shim(response: &str) -> (tempfile::TempDir, TmuxBackend) {
    let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(&format!(
        r#"#!/bin/sh
dir=$(dirname "$0")
[ "$4" = display-message ] || exit 0
count=0
if [ -f "$dir/reads" ]; then read -r count < "$dir/reads"; fi
count=$((count + 1))
printf '%s\n' "$count" > "$dir/reads"
{response}
"#
    ));
    let mut backend = TmuxBackend::with_socket(temp.path().join("server"));
    backend.program = Some(shim);
    (temp, backend)
}

#[cfg(unix)]
fn birth_cwd_read_count(temp: &tempfile::TempDir) -> usize {
    std::fs::read_to_string(temp.path().join("reads"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[cfg(unix)]
#[test]
fn birth_cwd_raced_reads_pass_once_the_path_settles() {
    let (temp, backend) =
        birth_cwd_shim(r#"if [ "$count" -le 3 ]; then printf '/\n'; else printf '/project\n'; fi"#);
    let result = backend.ensure_session(&session_options());
    assert!(
        result.is_ok(),
        "a transient cwd must not refuse birth: {result:?}"
    );
    assert_eq!(birth_cwd_read_count(&temp), 4);
}

#[cfg(unix)]
#[test]
fn birth_cwd_healthy_or_empty_read_is_single() {
    for response in ["printf '/project\\n'", ":"] {
        let (temp, backend) = birth_cwd_shim(response);
        let result = backend.ensure_session(&session_options());
        assert!(
            result.is_ok(),
            "a matching or unknown cwd passes: {result:?}"
        );
        assert_eq!(birth_cwd_read_count(&temp), 1);
    }
}

#[cfg(unix)]
#[test]
fn birth_cwd_stranding_server_refuses_after_the_bound() {
    let (temp, backend) = birth_cwd_shim("printf '/\\n'");
    let bound = Duration::from_millis(200);
    let started = Instant::now();
    let result = backend.verify_birth_cwd_within("rimz-test", Path::new("/project"), bound);
    assert!(started.elapsed() >= bound, "refused before the bound");
    let MuxErr::ServerCwdUnusable {
        session,
        requested,
        actual,
        socket,
    } = result.expect_err("a persistent mismatch must refuse birth")
    else {
        panic!("expected ServerCwdUnusable");
    };
    assert_eq!(session, "rimz-test");
    assert_eq!(requested, Path::new("/project"));
    assert_eq!(actual, Path::new("/"));
    assert_eq!(socket, backend.socket);
    assert!(birth_cwd_read_count(&temp) > 1);
}

#[cfg(unix)]
#[test]
fn birth_cwd_empty_reread_passes() {
    let (temp, backend) = birth_cwd_shim(r#"if [ "$count" = 1 ]; then printf '/\n'; fi"#);
    let result = backend.ensure_session(&session_options());
    assert!(
        result.is_ok(),
        "an unreadable reread is unknown, not unsafe: {result:?}"
    );
    assert_eq!(birth_cwd_read_count(&temp), 2);
}

#[cfg(unix)]
#[test]
fn birth_cwd_slow_first_read_still_gets_a_reread() {
    let (temp, backend) = birth_cwd_shim(
        r#"if [ "$count" = 1 ]; then sleep 0.1; printf '/\n'; else printf '/project\n'; fi"#,
    );
    let result = backend.verify_birth_cwd_within(
        "rimz-test",
        Path::new("/project"),
        Duration::from_millis(25),
    );
    assert!(
        result.is_ok(),
        "a slow first read must not spend the reread bound: {result:?}"
    );
    assert_eq!(birth_cwd_read_count(&temp), 2);
}

#[cfg(unix)]
#[test]
fn birth_cwd_reread_spawn_failure_propagates() {
    let (temp, backend) = birth_cwd_shim("rm \"$0\"; printf '/\\n'");
    let result =
        backend.verify_birth_cwd_within("rimz-test", Path::new("/project"), Duration::ZERO);
    assert!(
        matches!(result, Err(MuxErr::NotInstalled { .. })),
        "a reread spawn failure must propagate: {result:?}"
    );
    assert_eq!(birth_cwd_read_count(&temp), 1);
}

#[cfg(unix)]
#[test]
fn ensure_session_does_not_ignore_global_environment_repair_failure() {
    let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(
        "#!/bin/sh\nif [ \"$4\" = set-environment ] && [ \"$5\" = -g ]; then printf 'error connecting to server (Permission denied)\\n' >&2; exit 1; fi\n",
    );
    let mut backend = TmuxBackend::with_socket(temp.path().join("server"));
    backend.program = Some(shim);
    let opts = session_options();
    assert!(
        matches!(
            backend.ensure_session(&opts),
            Err(MuxErr::SocketConnectDenied { .. })
        ),
        "only the pre-birth no-server error may be ignored"
    );
}

#[cfg(unix)]
#[test]
fn session_listing_preserves_socket_connect_errors() {
    let cases = [
        ("no server running on /socket with spaces", "absent"),
        (
            "error connecting to /socket with spaces (No such file or directory)",
            "absent",
        ),
        (
            "error connecting to /socket with spaces (Permission denied)",
            "Permission denied",
        ),
        (
            "error connecting to /socket with spaces (Operation not permitted)",
            "Operation not permitted",
        ),
        (
            "error connecting to /socket with spaces (Too many open files)",
            "command",
        ),
    ];
    let results: Vec<_> = cases
        .into_iter()
        .map(|(stderr, expected)| {
            let (_temp, shim) = crate::mux::zellij::tests::support::zellij_shim(&format!(
                "#!/bin/sh\nprintf '%s\\n' '{stderr}' >&2\nexit 1\n"
            ));
            let mut backend = TmuxBackend::with_socket("/socket with spaces");
            backend.program = Some(shim);
            let result = backend.list_sessions_within(Duration::from_secs(1));
            let correct = match &result {
                Ok(sessions) => expected == "absent" && sessions.is_empty(),
                Err(MuxErr::Command { .. }) => expected == "command",
                Err(MuxErr::SocketConnectDenied {
                    mux,
                    socket,
                    reason,
                }) => {
                    *mux == MuxName::Tmux
                        && socket == Path::new("/socket with spaces")
                        && reason == expected
                }
                Err(_) => false,
            };
            (stderr, correct, result)
        })
        .collect();
    assert!(
        results.iter().all(|(_, correct, _)| *correct),
        "{results:?}"
    );

    let (_temp, shim) = crate::mux::zellij::tests::support::zellij_shim(
        "#!/bin/sh\nprintf 'other-room\\nrimz-test\\n'\n",
    );
    let mut backend = TmuxBackend::with_socket("/test/socket");
    backend.program = Some(shim);
    assert_eq!(
        backend.list_sessions().unwrap(),
        ["other-room", "rimz-test"]
    );
    assert_eq!(
        backend.session_liveness("rimz-test").unwrap(),
        crate::mux::SessionLiveness::Live
    );
    assert_eq!(
        backend.session_liveness("missing").unwrap(),
        crate::mux::SessionLiveness::Absent
    );
}

#[cfg(unix)]
#[test]
fn session_liveness_does_not_fold_socket_denial_into_absence() {
    let (_temp, shim) = crate::mux::zellij::tests::support::zellij_shim(
        "#!/bin/sh\nprintf 'error connecting to /test/socket (Operation not permitted)\\n' >&2\nexit 1\n",
    );
    let mut backend = TmuxBackend::with_socket("/test/socket");
    backend.program = Some(shim);
    assert!(matches!(
        backend.session_liveness("rimz-test"),
        Err(MuxErr::SocketConnectDenied { .. })
    ));
}

#[cfg(unix)]
#[test]
fn kill_session_only_tolerates_missing_sessions_and_servers() {
    let cases = [
        ("can't find session: rimz-test", "absent"),
        ("no server running on /test/socket", "absent"),
        (
            "error connecting to /test/socket (No such file or directory)",
            "absent",
        ),
        (
            "error connecting to /test/socket (Permission denied)",
            "Permission denied",
        ),
        (
            "error connecting to /test/socket (Operation not permitted)",
            "Operation not permitted",
        ),
        (
            "error connecting to /test/socket (Too many open files)",
            "command",
        ),
    ];
    let results: Vec<_> = cases
        .into_iter()
        .map(|(stderr, expected)| {
            let (_temp, shim) = crate::mux::zellij::tests::support::zellij_shim(&format!(
                "#!/bin/sh\nprintf '%s\\n' \"{stderr}\" >&2\nexit 1\n"
            ));
            let mut backend = TmuxBackend::with_socket("/test/socket");
            backend.program = Some(shim);
            let result = backend.kill_session("rimz-test");
            let correct = match &result {
                Ok(()) => expected == "absent",
                Err(MuxErr::Command { .. }) => expected == "command",
                Err(MuxErr::SocketConnectDenied {
                    mux,
                    socket,
                    reason,
                }) => {
                    *mux == MuxName::Tmux
                        && socket == Path::new("/test/socket")
                        && reason == expected
                }
                Err(_) => false,
            };
            (stderr, correct, result)
        })
        .collect();
    assert!(
        results.iter().all(|(_, correct, _)| *correct),
        "{results:?}"
    );
}

#[test]
fn rename_tab_targets_the_anchor_pane_and_encodes_the_name() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
    let spec = backend
        .rename_window_command(&pane, "#feat:one.2 ✓")
        .expect("tmux pane");

    assert_eq!(
        verb_args(&spec),
        ["rename-window", "-t", "%7", "##feat-one-2 ✓"]
    );
}

#[test]
fn tab_claim_pins_the_sanitized_pane_name_not_the_tab_name() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
    let claim = backend
        .claim_window_command(&pane, "#feat:one.2", "opus.fast")
        .expect("tmux pane");
    assert_eq!(
        verb_args(&claim),
        [
            "set-option",
            "-p",
            "-t",
            "%7",
            "@rimz_title",
            "opus-fast",
            ";",
            "rename-window",
            "-t",
            "%7",
            "##feat-one-2",
            ";",
            "set-option",
            "-w",
            "-t",
            "%7",
            "@rimz_tab_base",
            "#feat-one-2",
            ";",
            "set-option",
            "-w",
            "-t",
            "%7",
            "@rimz_tab_founders",
            "%7",
        ]
    );
}

#[test]
fn tab_release_lists_the_anchor_window_and_clears_each_pane_pin() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
    let list = backend.window_pane_ids_command(&pane).expect("tmux pane");
    assert_eq!(
        verb_args(&list),
        ["list-panes", "-t", "%7", "-F", "#{pane_id}"]
    );
    let release = backend
        .release_window_command(&pane, "zsh", &["%7".to_owned(), "%8".to_owned()])
        .expect("tmux pane");
    assert_eq!(
        verb_args(&release)[15..],
        [
            "@rimz_tab_base",
            ";",
            "set-option",
            "-wu",
            "-t",
            "%7",
            "@rimz_tab_founders",
            ";",
            "set-option",
            "-pu",
            "-t",
            "%7",
            "@rimz_title",
            ";",
            "set-option",
            "-pu",
            "-t",
            "%8",
            "@rimz_title",
        ]
    );
}

#[test]
fn rebuild_updates_the_base_inside_the_observed_name_guard() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
    let rebuild = backend
        .rebuild_window_command(&pane, "peer.fast ?", "peer-fast")
        .expect("tmux pane");
    let guarded = backend
        .window_name_guarded_command(&pane, "founder", "peer.fast ?", &rebuild)
        .expect("guard");
    assert_eq!(
        verb_args(&guarded)[19],
        "'rename-window' '-t' '%7' '#{@rimz_rename_target}' ; 'set-option' '-w' '-t' '%7' '@rimz_tab_base' 'peer-fast'"
    );
}

#[test]
fn release_clears_ownership_inside_the_observed_name_guard() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
    let release = backend
        .release_window_command(&pane, "sh", &["%7".to_owned()])
        .expect("release");
    let guarded = backend
        .window_name_guarded_command(&pane, "opus", "sh", &release)
        .expect("guard");
    let command = &verb_args(&guarded)[19];
    assert!(
        command.contains("'set-option' '-wu' '-t' '%7' '@rimz_tab_base'"),
        "{command}"
    );
    assert!(
        command.contains("'set-option' '-wu' '-t' '%7' '@rimz_tab_founders'"),
        "{command}"
    );
}

#[test]
fn projected_rename_runs_under_one_window_name_check() {
    let backend = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server");
    let pane = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
    for (observed, name) in [
        ("#feat ?", "#feat"),
        ("a_b", "a_b ✓"),
        ("x}y", "{x}"),
        ("it's ⢿", "it's"),
    ] {
        let rename = backend
            .rename_window_command(&pane, name)
            .expect("tmux pane");
        let guarded = backend
            .window_name_guarded_command(&pane, observed, name, &rename)
            .expect("guard");
        let target_name = name.replace([':', '.'], "-");
        assert_eq!(
            verb_args(&guarded),
            [
                "set-option",
                "-w",
                "-t",
                "%7",
                "@rimz_rename_observed",
                observed,
                ";",
                "set-option",
                "-w",
                "-t",
                "%7",
                "@rimz_rename_target",
                target_name.as_str(),
                ";",
                "if-shell",
                "-F",
                "-t",
                "%7",
                "#{==:#{window_name},#{@rimz_rename_observed}}",
                "'rename-window' '-t' '%7' '#{@rimz_rename_target}'",
                ";",
                "set-option",
                "-wu",
                "-t",
                "%7",
                "@rimz_rename_observed",
                ";",
                "set-option",
                "-wu",
                "-t",
                "%7",
                "@rimz_rename_target",
            ],
            "names travel as option values, never as command syntax"
        );
    }
}

#[test]
fn readonly_attach_blocks_input_and_ignores_viewer_size() {
    let spec = TmuxBackend::with_socket("/run/user/1000/rimz/tmux/server")
        .attach_readonly_command("rimz-test");

    assert_eq!(
        verb_args(&spec),
        ["attach", "-t", "rimz-test", "-r", "-f", "ignore-size"]
    );
}

#[test]
fn the_managed_endpoint_needs_no_workspace_to_reconstruct() {
    // Any caller rebuilds the same endpoint from the runtime domain alone —
    // this is what keeps `backend_for(MuxName)` free of a workspace argument.
    let runtime_root = Path::new("/run/user/1000");
    assert_eq!(
        managed_server_socket_path_under(runtime_root),
        PathBuf::from("/run/user/1000/rimz/tmux/server"),
    );
    // A disposable runtime root yields a private server, which is what gives
    // sandboxes and tests their isolation for free.
    assert_ne!(
        managed_server_socket_path_under(Path::new("/tmp/rimz-sandbox/runtime")),
        managed_server_socket_path_under(runtime_root),
    );
}

#[test]
fn legacy_conflict_recovery_is_scoped_to_the_one_session() {
    let conflict = LegacySessionConflict {
        session: "rimz-project-a1b2c3".to_owned(),
        socket: PathBuf::from("/tmp/tmux-1000/default"),
    };

    // Session-scoped on purpose: `kill-server` here would destroy the user's
    // own unrelated sessions, which RimZ does not own.
    assert_eq!(
        conflict.recovery_command(),
        "tmux -S /tmp/tmux-1000/default kill-session -t rimz-project-a1b2c3",
    );
    assert!(!conflict.recovery_command().contains("kill-server"));
}

#[test]
fn default_server_socket_path_uses_tmux_default_layout() {
    assert_eq!(
        default_server_socket_path_from(Path::new("/tmp"), 1001),
        PathBuf::from("/tmp/tmux-1001/default"),
    );
}

#[test]
fn tmux_var_parser_extracts_a_nonempty_socket() {
    assert_eq!(
        socket_path_from_tmux_var("/tmp/tmux-1001/default,42,3"),
        Some(PathBuf::from("/tmp/tmux-1001/default")),
    );
    assert_eq!(
        socket_path_from_tmux_var("/tmp/tmux-1001/default"),
        Some(PathBuf::from("/tmp/tmux-1001/default")),
    );
    assert_eq!(
        socket_path_from_tmux_var(" /tmp/tmux-1001/default ,42,3"),
        Some(PathBuf::from("/tmp/tmux-1001/default")),
    );
    assert_eq!(socket_path_from_tmux_var(",42,3"), None);
    assert_eq!(socket_path_from_tmux_var(""), None);
}

#[test]
fn equal_row_splits_size_each_remaining_stack() {
    let sizes = |pane_count| {
        (1..pane_count)
            .map(|index| window::equal_row_split_size(pane_count, index))
            .collect::<Vec<_>>()
    };

    assert_eq!(sizes(2), ["50%"]);
    assert_eq!(sizes(3), ["66%", "50%"]);
    assert_eq!(sizes(4), ["75%", "66%", "50%"]);
}

#[test]
fn even_column_heights_reserve_separators_and_distribute_remainder_last() {
    assert_eq!(window::even_column_heights(50, 3), [16, 16, 16]);
    assert_eq!(window::even_column_heights(51, 3), [16, 16, 17]);
    assert_eq!(window::even_column_heights(52, 3), [16, 17, 17]);
}

#[test]
fn even_split_sizes_hand_each_split_the_cells_its_remaining_panes_need() {
    // A chain of splits leaves panes of `even_column_heights`, separators
    // included: 3 columns over 181 cells are 59/60/60, 2 rows over 66 are
    // 32/33, and a run with nothing to split needs no size.
    assert_eq!(window::even_split_sizes(181, 3), [121, 60]);
    assert_eq!(window::even_split_sizes(66, 2), [33]);
    assert_eq!(window::even_split_sizes(50, 3), [33, 16]);
    assert!(window::even_split_sizes(50, 1).is_empty());
    // An extent too small for its panes still returns sizes instead of
    // tripping `even_column_heights`'s debug assertion; tmux clamps a split it
    // cannot fit and errors only once no space is left.
    assert_eq!(window::even_split_sizes(1, 3), [2, 1]);
}

#[test]
fn version_parser_and_floor_hold() {
    assert_eq!(parse_version("tmux 3.5a"), Some((3, 5, 0)));
    assert_eq!(parse_version("tmux 3.2"), Some((3, 2, 0)));
    assert_eq!(parse_version("  tmux 3.4  \n"), Some((3, 4, 0)));
    assert_eq!(parse_version("tmux 2.9a"), Some((2, 9, 0)));
    assert_eq!(parse_version("garbage"), None);

    assert!((3, 5, 0) >= MIN_TMUX_VERSION);
    assert!((3, 6, 0) >= MIN_TMUX_VERSION);
    // 3.4 lacks `extended-keys-format`, which the room options still set
    // across all supported hosts — below the floor.
    assert!((3, 4, 0) < MIN_TMUX_VERSION);
    assert!((3, 2, 0) < MIN_TMUX_VERSION);
}

#[test]
fn tmux_extended_key_bindings_follow_extended_key_format() {
    let csi_u = crate::config::TmuxConfig {
        extended_keys_format: crate::config::TmuxExtendedKeysFormat::CsiU,
        ..Default::default()
    };
    assert_eq!(
        options::tmux_extended_key_bindings(&csi_u),
        vec![
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                "S-Enter".to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
                "[13;2u".to_owned(),
            ],
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                "M-Enter".to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
                "[13;3u".to_owned(),
            ],
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                "User240".to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
            ],
        ],
    );

    let xterm = crate::config::TmuxConfig {
        extended_keys_format: crate::config::TmuxExtendedKeysFormat::Xterm,
        ..Default::default()
    };
    assert_eq!(
        options::tmux_extended_key_bindings(&xterm),
        vec![
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                "S-Enter".to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
                "[27;2;13~".to_owned(),
            ],
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                "M-Enter".to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
                "[27;3;13~".to_owned(),
            ],
            vec![
                "bind-key".to_owned(),
                "-n".to_owned(),
                "User240".to_owned(),
                "send-keys".to_owned(),
                "Escape".to_owned(),
            ],
        ],
    );

    let disabled = crate::config::TmuxConfig {
        extended_keys: false,
        ..Default::default()
    };
    assert!(options::tmux_extended_key_bindings(&disabled).is_empty());
}

#[test]
fn window_name_neutralizes_tmux_target_separators() {
    use super::window::sanitize_window_name;

    // tmux parses `:` as session:window and `.` as window.pane in a target
    // spec, so `new-window -n` rejects a name carrying either. The run-pane
    // title and channel labels are human text that can carry both.
    assert_eq!(sanitize_window_name("run: codex"), "run- codex");
    assert_eq!(sanitize_window_name("feat: split.ci"), "feat- split-ci");
    assert_eq!(sanitize_window_name("plain-name"), "plain-name");
}

#[test]
fn window_name_arg_encodes_literal_hashes_after_sanitizing() {
    use super::window::window_name_arg;

    assert_eq!(window_name_arg("#feat: split.ci##"), "##feat- split-ci####");
    assert_eq!(window_name_arg("plain-name"), "plain-name");
}

#[test]
fn open_tab_rejects_an_empty_layout() {
    use std::path::{Path, PathBuf};

    use crate::ids::WorkspaceId;
    use crate::mux::{
        LayoutColumn, LayoutPanes, MuxBackend, MuxErr, PaneCmd, SidebarPaneOptions, TabOptions,
    };

    // Pointed at a socket no server owns: the empty-layout guards return before
    // any tmux command runs, so this never forks tmux and needs no live server.
    let backend = TmuxBackend::with_socket("/nonexistent/rimz-open-tab.sock");
    let sidebar = SidebarPaneOptions {
        runtime: crate::disk::paths::RuntimePaths::under(
            WorkspaceId::from_project_root(Path::new("/tmp/rimz-empty")),
            tempfile::tempdir().expect("runtime root").path(),
        )
        .expect("runtime paths"),
        session_name: "rimz-empty".to_owned(),
        workspace_id: WorkspaceId::from_project_root(Path::new("/tmp/rimz-empty")),
        project_root: PathBuf::from("/tmp/rimz-empty"),
        extra_env: Default::default(),
        cwd: PathBuf::from("/tmp/rimz-empty"),
        target: crate::mux::SidebarTarget {
            share: crate::mux::WidthPermille::from_percent(25),
            max_cols: std::num::NonZeroU16::new(20).expect("nonzero test width"),
            pinned: false,
        },
        detected_view_size: None,
        rimz_bin: PathBuf::from("/bin/true"),
        pristine_birth: false,
        config: crate::config::MultiplexerConfig::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let tab = |columns: Vec<Vec<PaneCmd>>| TabOptions {
        env: Default::default(),
        title: "work".to_owned(),
        panes: LayoutPanes {
            columns: columns
                .into_iter()
                .map(|panes| LayoutColumn {
                    panes,
                    stacked: false,
                })
                .collect(),
            focused_pane: 0,
        },
        focus: true,
        dock_sidebar: true,
        after: None,
        sidebar: sidebar.clone(),
    };

    let err = backend
        .open_tab(&tab(Vec::new()))
        .expect_err("no columns must error");
    assert!(
        matches!(err, MuxErr::Output { ref reason, .. } if reason.contains("no columns")),
        "expected a no-columns Output error, got {err:?}",
    );

    let err = backend
        .open_tab(&tab(vec![Vec::new()]))
        .expect_err("an empty column must error");
    assert!(
        matches!(err, MuxErr::Output { ref reason, .. } if reason.contains("empty column")),
        "expected an empty-column Output error, got {err:?}",
    );
}

#[cfg(unix)]
#[test]
fn new_window_anchors_stay_in_the_target_session() {
    for (session, anchor_session, missing, anchored) in [
        ("room", "other-room", false, false),
        ("room", "room", false, true),
        ("room with spaces", "room with spaces", false, true),
        ("room", "", true, false),
    ] {
        let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(&format!(
            r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$@" >> "$dir/argv"
if [ "$4" = display-message ]; then
    if [ '{missing}' = true ]; then exit 1; fi
    if [ "$8" = '#{{window_id}}' ]; then
        printf '@17\n'
    else
        printf '%s\n' '{anchor_session} @17'
    fi
elif [ "$4" = new-window ]; then
    printf '@23 %%24\n'
fi
"#
        ));
        let mut backend = TmuxBackend::with_socket(temp.path().join("server"));
        backend.program = Some(shim);
        let anchor = crate::PaneId::from_parts(crate::MuxName::Tmux, "%7");
        let opened = backend
            .open_named_window(
                session,
                "new",
                Path::new("/project"),
                &["true".to_owned()],
                Some(&anchor),
                &Default::default(),
            )
            .expect("an unusable anchor must not fail the launch");
        assert_eq!(opened.window_id, "@23");
        assert_eq!(opened.first_pane, "%24");
        let log = std::fs::read_to_string(temp.path().join("argv")).unwrap();
        let args = log.lines().collect::<Vec<_>>();
        let new_window = args.iter().position(|arg| *arg == "new-window").unwrap();
        let command = &args[new_window..];
        let target = command.iter().position(|arg| *arg == "-t").unwrap() + 1;
        assert_eq!(command[target], if anchored { "@17" } else { session });
        assert_eq!(command.contains(&"-a"), anchored);
    }
}

#[test]
fn version_serves_the_memoized_probe() {
    let backend = TmuxBackend::default();
    backend
        .version
        .set("tmux 9.9".to_owned())
        .expect("a fresh instance has not probed yet");
    // The cache is consulted before any probe: the seeded value comes back
    // verbatim — no `tmux -V` fork, no overwrite by a real binary.
    assert_eq!(backend.version().expect("cached version"), "tmux 9.9");
}

#[test]
fn list_panes_scopes_session_without_server_wide_flag() {
    let backend = TmuxBackend::default();

    let session_spec = backend.list_panes_command(Some("rimz-room"));
    let session_args = verb_args(&session_spec);
    assert_eq!(
        &session_args[..5],
        ["list-panes", "-s", "-t", "rimz-room", "-F"]
    );
    assert!(!session_args.iter().any(|arg| arg == "-a"));
    assert!(session_args[5].contains(
        ",#{s/,/_/g:#{?#{@rimz_title},#{@rimz_title},#{pane_title}}},#{pane_floating_flag},"
    ));

    let server_spec = backend.list_panes_command(None);
    assert_eq!(&verb_args(&server_spec)[..3], ["list-panes", "-a", "-F"]);
}

#[cfg(unix)]
#[test]
fn reconcile_samples_liveness_once_after_the_native_listing() {
    use crate::mux::{SidebarLiveness, SidebarLivenessSource, SidebarPaneOptions};
    use std::cell::Cell;

    struct AfterListing<'a> {
        temp: &'a tempfile::TempDir,
        samples: Cell<usize>,
    }
    impl SidebarLivenessSource for AfterListing<'_> {
        fn sample(&self) -> SidebarLiveness {
            self.samples.set(self.samples.get() + 1);
            let log = std::fs::read_to_string(self.temp.path().join("argv")).unwrap_or_default();
            let mut live = SidebarLiveness::default();
            if log.lines().any(|line| line == "list-panes") {
                live.young_panes
                    .insert(crate::PaneId::from_parts(MuxName::Tmux, "%1"));
            }
            live
        }
    }

    let (temp, shim) = crate::mux::zellij::tests::support::zellij_shim(
        r#"#!/bin/sh
dir=$(dirname "$0")
printf '%s\n' "$4" >> "$dir/argv"
if [ "$4" = list-panes ]; then
  printf '%s\n' 'rimz-test,@0,%1,rimz,/,1,work,rimz-sidebar,0' 'rimz-test,@0,%2,sh,/,2,work,shell,0'
fi
"#,
    );
    let mut backend = TmuxBackend::with_socket(temp.path().join("server"));
    backend.program = Some(shim.clone());
    let workspace_id = crate::WorkspaceId::from_project_root(temp.path());
    let opts = SidebarPaneOptions {
        runtime: crate::RuntimePaths::under(workspace_id.clone(), temp.path()).unwrap(),
        session_name: "rimz-test".to_owned(),
        workspace_id,
        project_root: temp.path().to_path_buf(),
        extra_env: Default::default(),
        cwd: temp.path().to_path_buf(),
        target: crate::mux::SidebarTarget {
            share: crate::mux::WidthPermille::from_percent(25),
            max_cols: std::num::NonZeroU16::new(72).unwrap(),
            pinned: false,
        },
        detected_view_size: None,
        rimz_bin: shim,
        pristine_birth: false,
        config: Default::default(),
        resume_tabs: Vec::new(),
        refresh_ms: None,
    };
    let live = AfterListing {
        temp: &temp,
        samples: Cell::new(0),
    };
    let report = backend.reconcile_sidebars(&opts, &live).unwrap();
    let log = std::fs::read_to_string(temp.path().join("argv")).unwrap();
    assert_eq!(live.samples.get(), 1);
    assert!(
        !log.contains("split-window") && !log.contains("kill-pane"),
        "{log}"
    );
    assert_eq!(log.lines().filter(|line| *line == "list-panes").count(), 1);
    assert_eq!(report, crate::mux::SidebarRecovery::default());
}

#[test]
fn client_view_uses_a_printable_field_separator() {
    let spec = TmuxBackend::default().client_view_command(Some("rimz-room"));
    assert_eq!(
        verb_args(&spec),
        [
            "list-clients",
            "-F",
            "#{client_name}|#{pane_id}|#{client_activity}|#{client_flags}",
            "-t",
            "rimz-room",
        ],
    );
}

#[test]
fn sidebar_geometry_probe_is_one_session_scoped_command() {
    let backend = TmuxBackend::default();

    let spec = backend.session_pane_geometries_command("rimz-room");
    assert_eq!(
        verb_args(&spec),
        [
            "list-panes",
            "-s",
            "-t",
            "rimz-room",
            "-F",
            "#{pane_id} #{window_id} #{pane_width} #{window_width} #{==:#{pane_title},rimz-sidebar}",
        ],
    );
}

#[test]
fn sidebar_geometry_probe_parser_requires_five_typed_fields() {
    use super::window::{TmuxPaneGeometry, parse_tmux_pane_geometry};

    assert_eq!(
        parse_tmux_pane_geometry("%3 @1 72 240 1"),
        Some(TmuxPaneGeometry {
            pane_id: "%3".to_owned(),
            window_id: "@1".to_owned(),
            pane_width: 72,
            window_width: 240,
            is_sidebar: true,
        }),
    );
    assert_eq!(parse_tmux_pane_geometry("%3 @1 wide 240 1"), None);
    assert_eq!(parse_tmux_pane_geometry("%3 @1 72 240 0 extra"), None);
}
