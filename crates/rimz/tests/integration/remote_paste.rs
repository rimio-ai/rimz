//! Remote paste through the keyboard relay: Ctrl+V with files or an image on
//! the laptop's clipboard, or a dropped path list, uploads into the room and
//! types the remote paths into the session as a drop would.
//!
//! The clipboard is a `RIMZ_REMOTE_CLIPBOARD_BIN` stub, and the ssh-trace shim
//! runs the real receiver on the upload channel.

use super::keyboard::{Attach, SENTINEL, recording_command, stdin_log};
use super::upload::{lane, room, upload_units};
use super::*;

use rimz::remote::paste::{drop_paste, encode_drop};

const CTRL_V: &[u8] = b"\x16";
const CTRL_V_CSI_U: &[u8] = b"\x1b[118;5u";
const CTRL_V_MODIFY_OTHER_KEYS: &[u8] = b"\x1b[27;5;118~";

fn clipboard_stub(env: &Env, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt as _;

    let path = env.project_root.join("clipboard-stub");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write clipboard stub");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod clipboard stub");
    path
}

/// A clipboard stub that holds `files` as Finder-copied file URLs.
fn files_stub(env: &Env, files: &[&Path]) -> PathBuf {
    let lines = files
        .iter()
        .map(|file| format!("printf '%s\\n' '{}'", file.display()))
        .collect::<Vec<_>>()
        .join("\n");
    clipboard_stub(env, &format!("echo files\n{lines}"))
}

fn local_file(env: &Env, name: &str, bytes: &[u8]) -> PathBuf {
    let path = env.project_root.join("local").join(name);
    std::fs::create_dir_all(path.parent().unwrap()).expect("mkdir local files");
    std::fs::write(&path, bytes).expect("write local file");
    path
}

struct Setup<'a> {
    clipboard: Option<&'a Path>,
    receiver: bool,
    stall: bool,
}

fn paste_command(env: &Env, log: &Path, setup: Setup<'_>) -> CommandBuilder {
    let mut cmd = recording_command(env, log);
    match setup.clipboard {
        Some(stub) => cmd.env("RIMZ_REMOTE_CLIPBOARD_BIN", stub),
        None => cmd.env_remove("RIMZ_REMOTE_CLIPBOARD_BIN"),
    }
    if setup.receiver {
        cmd.env("RIMZ_TEST_SSH_UPLOAD_RIMZ", env.rimz_bin());
        cmd.env("RIMZ_TEST_SSH_UPLOAD_DIR", &env.project_root);
    }
    if setup.stall {
        cmd.env("RIMZ_TEST_SSH_UPLOAD_STALL", "1");
    }
    cmd
}

fn start(env: &Env, setup: Setup<'_>) -> Attach {
    let log = env.project_root.join("ssh-trace.log");
    let attach = Attach::start(remote_connect_pty(), paste_command(env, &log, setup));
    // Ctrl+V on a cooked terminal is the line discipline's literal-next key.
    attach.wait_for("the terminal never went raw", || {
        !read_tty_termios(&attach.tty_name)
            .local_flags
            .contains(LocalFlags::ICANON)
    });
    attach
}

fn recorded(env: &Env) -> Vec<u8> {
    std::fs::read(stdin_log(env)).unwrap_or_default()
}

fn pastes_recorded(env: &Env) -> usize {
    recorded(env)
        .windows(b"\x1b[201~".len())
        .filter(|window| *window == b"\x1b[201~")
        .count()
}

fn only_unit(state: &rimz::StatePaths) -> PathBuf {
    let units = upload_units(state);
    assert_eq!(units.len(), 1, "one temp unit per transfer: {units:?}");
    units[0].clone()
}

fn remote_paste(paths: &[PathBuf]) -> Vec<u8> {
    let paths = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    drop_paste(&paths)
}

fn assert_recorded(env: &Env, expected: &[u8]) {
    assert_eq!(
        String::from_utf8_lossy(&recorded(env)),
        String::from_utf8_lossy(expected)
    );
}

#[test]
fn ctrl_v_with_copied_files_uploads_them_and_pastes_their_remote_paths() {
    let env = Env::new();
    let state = room(&env);
    let a = local_file(&env, "a.txt", b"alpha");
    let b = local_file(&env, "b c.txt", b"beta");
    let stub = files_stub(&env, &[&a, &b]);
    let attach = start(
        &env,
        Setup {
            clipboard: Some(&stub),
            receiver: true,
            stall: false,
        },
    );

    attach.terminal.write(CTRL_V);
    attach.wait_for("no paste reached the session", || {
        pastes_recorded(&env) == 1
    });
    attach.terminal.write(SENTINEL.as_bytes());
    attach.finish();

    let unit = only_unit(&state);
    let remote = [unit.join("a.txt"), unit.join("b c.txt")];
    assert_recorded(
        &env,
        &[remote_paste(&remote), SENTINEL.as_bytes().to_vec()].concat(),
    );
    assert_eq!(std::fs::read(&remote[0]).unwrap(), b"alpha");
    assert_eq!(std::fs::read(&remote[1]).unwrap(), b"beta");
}

#[test]
fn ctrl_v_with_an_image_uploads_one_png_and_removes_the_local_copy() {
    let env = Env::new();
    let state = room(&env);
    let written = env.project_root.join("clipboard-png-path");
    let stub = clipboard_stub(
        &env,
        &format!(
            "printf PNG > \"$1\"\nprintf '%s' \"$1\" > '{}'\nprintf 'image\\n%s\\n' \"$1\"",
            written.display()
        ),
    );
    let attach = start(
        &env,
        Setup {
            clipboard: Some(&stub),
            receiver: true,
            stall: false,
        },
    );

    attach.terminal.write(CTRL_V_CSI_U);
    attach.wait_for("no paste reached the session", || {
        pastes_recorded(&env) == 1
    });
    attach.terminal.write(SENTINEL.as_bytes());
    attach.finish();

    let unit = only_unit(&state);
    let files = std::fs::read_dir(&unit)
        .expect("read the transfer's unit")
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("clipboard-") && name.ends_with(".png"),
        "{name}"
    );
    assert_eq!(std::fs::read(&files[0]).unwrap(), b"PNG");
    assert_recorded(
        &env,
        &[remote_paste(&files), SENTINEL.as_bytes().to_vec()].concat(),
    );
    let local = std::fs::read_to_string(&written).expect("the reader got a png path");
    assert!(!Path::new(&local).exists(), "{local} is left behind");
}

#[test]
fn ctrl_v_forwards_unchanged_when_there_is_nothing_to_upload() {
    let env = Env::new();
    let state = room(&env);
    let file = local_file(&env, "a.txt", b"alpha");
    let runs = env.project_root.join("clipboard-runs");
    for (case, body, receiver) in [
        (
            "an empty clipboard",
            Some(format!("echo run >> '{}'\necho none", runs.display())),
            true,
        ),
        ("a failing reader", Some("exit 3".to_owned()), true),
        ("a hung reader", Some("exec sleep 347".to_owned()), true),
        ("no reader", None, true),
        (
            "no receiver on the host",
            Some(format!("echo files\necho '{}'", file.display())),
            false,
        ),
    ] {
        let _ = std::fs::remove_file(stdin_log(&env));
        let stub = body.map(|body| clipboard_stub(&env, &body));
        let attach = start(
            &env,
            Setup {
                clipboard: stub.as_deref(),
                receiver,
                stall: false,
            },
        );
        // A held key reuses the empty verdict instead of reading again.
        attach
            .terminal
            .write(&[CTRL_V, CTRL_V, SENTINEL.as_bytes()].concat());
        attach.finish();

        assert_eq!(
            String::from_utf8_lossy(&recorded(&env)),
            String::from_utf8_lossy(&[CTRL_V, CTRL_V, SENTINEL.as_bytes()].concat()),
            "{case}"
        );
        assert!(upload_units(&state).is_empty(), "{case}");
    }
    assert_eq!(
        std::fs::read_to_string(&runs).unwrap_or_default(),
        "run\n",
        "the empty clipboard is read once for a held key"
    );
}

#[test]
fn a_receiver_failure_pastes_nothing_and_shows_the_failed_row() {
    let env = Env::new();
    let state = room(&env);
    std::fs::create_dir_all(env.rimz_home()).expect("mkdir rimz config dir");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        "[remote]\npaste_max = \"2B\"\n",
    )
    .expect("write remote config");
    let file = local_file(&env, "a.txt", b"alpha");
    let stub = files_stub(&env, &[&file]);
    let attach = start(
        &env,
        Setup {
            clipboard: Some(&stub),
            receiver: true,
            stall: false,
        },
    );

    attach.terminal.write(CTRL_V);
    attach.wait_for("the receiver published no failure", || {
        lane(&env).is_some_and(|record| record.state == rimz::remote::upload::UploadState::Failed)
    });
    attach.terminal.write(SENTINEL.as_bytes());
    attach.finish();

    assert_recorded(&env, SENTINEL.as_bytes());
    assert!(upload_units(&state).is_empty());
    assert_eq!(
        lane(&env).and_then(|record| record.reason).as_deref(),
        Some("5 B is over the 2 B upload limit (paste_max)")
    );
}

#[test]
fn a_dropped_path_list_uploads_and_any_other_paste_forwards() {
    let env = Env::new();
    let state = room(&env);
    let a = local_file(&env, "a.txt", b"alpha");
    let b = local_file(&env, "b (1).txt", b"beta");
    let local = |paths: &[&Path]| {
        paths
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    let missing = drop_paste(&local(&[&a, Path::new("/rimz-missing/b.txt")]));
    let attach = start(
        &env,
        Setup {
            clipboard: None,
            receiver: true,
            stall: false,
        },
    );

    attach.terminal.write(&missing);
    attach.terminal.write(&drop_paste(&local(&[&a, &b])));
    attach.wait_for("no remote paths reached the session", || {
        pastes_recorded(&env) == 2
    });
    attach.terminal.write(SENTINEL.as_bytes());
    attach.finish();

    let unit = only_unit(&state);
    let remote = [unit.join("a.txt"), unit.join("b (1).txt")];
    assert_recorded(
        &env,
        &[missing, remote_paste(&remote), SENTINEL.as_bytes().to_vec()].concat(),
    );
    assert!(encode_drop(&local(&[&b])).contains("\\ \\(1\\)"));
    assert_eq!(std::fs::read(&remote[1]).unwrap(), b"beta");
}

#[test]
fn a_running_upload_swallows_repeats_forwards_pastes_and_cancels_on_ctrl_v_or_session_end() {
    let env = Env::new();
    let state = room(&env);
    let file = local_file(&env, "a.txt", b"alpha");
    let stub = files_stub(&env, &[&file]);
    let paste = drop_paste(&[file.to_string_lossy().into_owned()]);
    let attach = start(
        &env,
        Setup {
            clipboard: Some(&stub),
            receiver: true,
            stall: true,
        },
    );
    let wait_units = |what: &str, present: bool| {
        attach.wait_for(what, || upload_units(&state).is_empty() != present);
    };

    // The trigger and a key repeat right behind it.
    attach
        .terminal
        .write(&[CTRL_V_MODIFY_OTHER_KEYS, CTRL_V].concat());
    wait_units("the upload never started", true);
    attach.terminal.write(&paste);
    std::thread::sleep(Duration::from_millis(1_200));
    attach.terminal.write(CTRL_V);
    wait_units("the cancelled upload left its unit", false);
    attach.terminal.write(CTRL_V);
    wait_units("the second upload never started", true);
    attach.terminal.write(SENTINEL.as_bytes());
    let mut attach = attach;
    assert!(attach.wait_exit().is_some_and(|status| status.success()));
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    while !upload_units(&state).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    drop(attach.terminal.finish());

    assert!(
        upload_units(&state).is_empty(),
        "the session's end cancels its upload"
    );
    assert_recorded(&env, &[paste, SENTINEL.as_bytes().to_vec()].concat());
}
