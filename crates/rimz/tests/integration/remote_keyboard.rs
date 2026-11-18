//! The keyboard relay in front of a supervised attach: ssh reads a pty that
//! rimz feeds, and the real terminal is raw exactly while ssh holds that pty
//! raw.

use super::*;

pub(super) const SENTINEL: &str = "RIMZ-RELAY-END";

/// A supervised attach running on a fake terminal.
pub(super) struct Attach {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    pub(super) terminal: FakeTerminal,
    pub(super) tty_name: PathBuf,
    deadline: Instant,
}

impl Attach {
    pub(super) fn start(pair: PtyPair, cmd: CommandBuilder) -> Self {
        let child = pair.slave.spawn_command(cmd).expect("spawn remote connect");
        let tty_name = pair.master.tty_name().expect("remote connect pty name");
        drop(pair.slave);
        Self {
            child,
            terminal: FakeTerminal::new(pair.master, Duration::ZERO, Answers::StatusOnly),
            tty_name,
            deadline: Instant::now() + COMMAND_TIMEOUT,
        }
    }

    pub(super) fn wait_for(&self, what: &str, ready: impl Fn() -> bool) {
        while !ready() && Instant::now() < self.deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            ready(),
            "{what}; output: {:?}",
            String::from_utf8_lossy(&self.terminal.output())
        );
    }

    pub(super) fn wait_exit(&mut self) -> Option<portable_pty::ExitStatus> {
        loop {
            if let Some(status) = self.child.try_wait().expect("poll remote connect") {
                return Some(status);
            }
            if Instant::now() >= self.deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub(super) fn finish(mut self) -> String {
        let status = self.wait_exit();
        let output = String::from_utf8_lossy(&self.terminal.finish()).into_owned();
        let status =
            status.unwrap_or_else(|| panic!("remote connect timed out; output:\n{output}"));
        assert!(
            status.success(),
            "remote connect failed with {status:?}; output:\n{output}"
        );
        output
    }
}

pub(super) fn recording_command(env: &Env, log: &Path) -> CommandBuilder {
    let stdin_plan = env.project_root.join("ssh-stdin.plan");
    std::fs::write(&stdin_plan, "record\n").expect("write ssh stdin plan");
    let mut cmd = remote_connect_pty_command(env, log);
    cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
    cmd.env("RIMZ_TEST_SSH_STDIN_PLAN", &stdin_plan);
    cmd.env("RIMZ_TEST_SSH_STDIN_LOG", stdin_log(env));
    cmd.env("RIMZ_TEST_SSH_STDIN_UNTIL", SENTINEL);
    cmd.env("RIMZ_TEST_SSH_TTY_NAME_LOG", tty_name_log(env));
    cmd
}

pub(super) fn stdin_log(env: &Env) -> PathBuf {
    env.project_root.join("ssh-stdin.log")
}

pub(super) fn tty_name_log(env: &Env) -> PathBuf {
    env.project_root.join("ssh-tty-name.log")
}

fn attach_tty_names(env: &Env) -> Vec<String> {
    std::fs::read_to_string(tty_name_log(env))
        .unwrap_or_default()
        .lines()
        .map(ToOwned::to_owned)
        .collect()
}

/// The attach read a pty of its own rather than the user's terminal.
fn assert_attach_read_a_relay_pty(env: &Env, terminal: &Path) {
    let names = attach_tty_names(env);
    assert_eq!(names.len(), 1, "one attach: {names:?}");
    assert!(
        names[0].starts_with("/dev/pts/") && Path::new(&names[0]) != terminal,
        "the attach's stdin is the relay's pty, not {}: {names:?}",
        terminal.display()
    );
}

pub(super) fn write_paste_config(env: &Env, paste: bool) {
    let dir = env.rimz_home();
    std::fs::create_dir_all(&dir).expect("mkdir rimz config dir");
    std::fs::write(
        dir.join("config.toml"),
        format!("[remote]\npaste = {paste}\n"),
    )
    .expect("write remote config");
}

#[test]
fn the_attach_reads_the_terminal_itself_only_with_paste_off() {
    for paste in [false, true] {
        let env = Env::new();
        write_paste_config(&env, paste);
        let log = env.project_root.join("ssh-trace.log");
        let pair = remote_connect_pty();
        let terminal = pair.master.tty_name().expect("remote connect pty name");
        let mut cmd = remote_connect_pty_command(&env, &log);
        cmd.env("RIMZ_TEST_SSH_TTY_NAME_LOG", tty_name_log(&env));
        let (output, settings) = run_pty_command(pair, cmd);

        assert_eq!(main_invocation_count(&log), 1, "one attach: {output}");
        if paste {
            assert_attach_read_a_relay_pty(&env, &terminal);
        } else {
            assert_eq!(
                attach_tty_names(&env),
                [terminal.display().to_string()],
                "paste = false leaves ssh on the user's terminal"
            );
        }
        assert_shell_tty(&settings);
    }
}

#[test]
fn input_typed_before_the_attach_goes_raw_arrives_once_after() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let pair = remote_connect_pty();
    let mut cmd = recording_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_RAW_DELAY_MS", "600");
    let attach = Attach::start(pair, cmd);

    attach.wait_for("the attach did not start", || {
        !attach_tty_names(&env).is_empty()
    });
    attach.terminal.write(SENTINEL.as_bytes());
    let terminal = attach.tty_name.clone();
    attach.finish();

    assert_attach_read_a_relay_pty(&env, &terminal);
    assert_eq!(
        std::fs::read(stdin_log(&env)).expect("read attach stdin"),
        SENTINEL.as_bytes(),
        "early input reaches the attach once"
    );
}

#[test]
fn leaving_raw_mid_session_returns_the_terminal_in_shell_modes() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let state_log = env.project_root.join("ssh-tty-state.log");
    let pair = remote_connect_pty();
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
    cmd.env("RIMZ_TEST_SSH_COOKED_AFTER_MS", "300");
    cmd.env("RIMZ_TEST_SSH_SLEEP_MS", "2000");
    cmd.env("RIMZ_TEST_SSH_TTY_STATE_LOG", &state_log);
    cmd.env("RIMZ_TEST_SSH_TTY_NAME_LOG", tty_name_log(&env));
    let attach = Attach::start(pair, cmd);

    attach.wait_for("the attach never left raw mode", || {
        std::fs::read_to_string(&state_log).is_ok_and(|log| log.contains("cooked"))
    });
    std::thread::sleep(Duration::from_millis(500));
    let mid_session = read_tty_termios(&attach.tty_name);
    let terminal = attach.tty_name.clone();
    attach.finish();

    assert_attach_read_a_relay_pty(&env, &terminal);
    assert_shell_tty(&mid_session);
}

#[test]
fn every_input_byte_is_forwarded_unchanged_and_in_order() {
    const INPUT: &[&[u8]] = &[
        b"\x1b[<0;10;5M\x1b[<0;10;5m",
        b"\x1b[I\x1b[O",
        b"\x1b[97;5u\x1b[13;2u",
        b"\x1b[12;5R",
        b"\x1b[200~hello world\x1b[201~",
        b"\x1b[200~/not/a/file\x1b[201~",
        b"plain text\r",
    ];

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let pair = remote_connect_pty();
    let attach = Attach::start(pair, recording_command(&env, &log));
    // Input that arrives while the terminal is cooked is translated on its
    // way in (ICRNL), whoever reads it later.
    attach.wait_for("the terminal never went raw", || {
        !read_tty_termios(&attach.tty_name)
            .local_flags
            .contains(LocalFlags::ICANON)
    });
    let mut expected = INPUT.concat();
    expected.extend_from_slice(SENTINEL.as_bytes());
    for byte in &expected {
        attach.terminal.write(std::slice::from_ref(byte));
    }
    let terminal = attach.tty_name.clone();
    attach.finish();

    assert_attach_read_a_relay_pty(&env, &terminal);
    assert_eq!(
        String::from_utf8_lossy(&std::fs::read(stdin_log(&env)).expect("read attach stdin")),
        String::from_utf8_lossy(&expected)
    );
}

#[test]
fn a_terminal_resize_reaches_the_attach() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let size_log = env.project_root.join("ssh-size.log");
    let winch_log = env.project_root.join("ssh-winch.log");
    let pair = remote_connect_pty();
    let mut cmd = recording_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_SIZE_LOG", &size_log);
    cmd.env("RIMZ_TEST_SSH_WINCH_LOG", &winch_log);
    let attach = Attach::start(pair, cmd);
    let lines = |path: &Path| {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>()
    };
    let sizes = || lines(&size_log);

    attach.wait_for("the attach recorded no size", || !sizes().is_empty());
    attach.terminal.resize(30, 120);
    attach.wait_for("the resize never reached the attach", || {
        sizes().iter().any(|size| size == "30 120")
    });
    // ssh learns of a new size only from SIGWINCH, so the pty's size alone
    // would leave the remote pane at the old one.
    attach.wait_for("the attach got no SIGWINCH at the new size", || {
        lines(&winch_log).iter().any(|size| size == "30 120")
    });
    attach.terminal.write(SENTINEL.as_bytes());
    let terminal = attach.tty_name.clone();
    attach.finish();

    assert_attach_read_a_relay_pty(&env, &terminal);
    assert_eq!(
        sizes()[0],
        "24 100",
        "the attach starts at the terminal's size"
    );
}

#[test]
fn input_typed_after_the_attach_exits_stays_queued_for_the_shell() {
    const TYPEAHEAD: &[u8] = b"rimz-shell-typeahead\n";

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let pid_file = env.project_root.join("ssh-trace.pid");
    let pair = remote_connect_pty();
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
    cmd.env("RIMZ_TEST_SSH_LATE_OUTPUT", "1");
    cmd.env("RIMZ_TEST_SSH_PID_FILE", &pid_file);
    let attach = Attach::start(pair, cmd);

    attach.wait_for("the attach did not start", || pid_file.exists());
    let pid = std::fs::read_to_string(&pid_file).expect("read attach pid");
    let proc = PathBuf::from(format!("/proc/{}", pid.trim()));
    attach.wait_for("the attach did not exit", || {
        std::fs::read_to_string(proc.join("stat")).map_or(true, |stat| stat.contains(") Z "))
    });
    attach.terminal.write(TYPEAHEAD);
    let mut attach = attach;
    let status = attach.wait_exit();
    assert!(
        status
            .as_ref()
            .is_some_and(portable_pty::ExitStatus::success),
        "remote connect failed with {status:?}"
    );

    let mut tty = std::fs::File::open(&attach.tty_name).expect("reopen remote connect pty");
    nix::fcntl::fcntl(
        &tty,
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )
    .expect("set remote connect pty nonblocking");
    let mut queued = vec![0u8; TYPEAHEAD.len()];
    tty.read_exact(&mut queued)
        .expect("read queued shell typeahead");
    assert_eq!(queued, TYPEAHEAD);
    drop(attach.terminal.finish());
}

#[test]
fn a_relay_error_while_the_attach_runs_ends_the_connect() {
    const RELAY_DEADLINE: Duration = Duration::from_secs(10);

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let pid_file = env.project_root.join("ssh-trace.pid");
    let status_file = env.project_root.join("rimz-status");
    let pair = remote_connect_pty();
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
    cmd.env("RIMZ_TEST_SSH_TTY_WRITE", "drawn on the attach's terminal");
    cmd.env("RIMZ_TEST_SSH_SLEEP_MS", "30000");
    cmd.env("RIMZ_TEST_SSH_PID_FILE", &pid_file);
    // rimz's stdout is a pipe whose reader has exited, so the relay's copy
    // of what the attach drew fails with EPIPE while ssh still runs.
    cmd.env("RIMZ_TEST_STATUS_FILE", &status_file);
    let wrapper = [
        "sh",
        "-c",
        r#"{ "$@"; echo $? > "$RIMZ_TEST_STATUS_FILE"; } | :; exit "$(cat "$RIMZ_TEST_STATUS_FILE")""#,
        "sh",
    ];
    cmd.get_argv_mut()
        .splice(0..0, wrapper.into_iter().map(Into::into));
    let mut attach = Attach::start(pair, cmd);
    attach.wait_for("the attach did not start", || pid_file.exists());
    let pid = std::fs::read_to_string(&pid_file).expect("read attach pid");
    let proc = PathBuf::from(format!("/proc/{}", pid.trim()));

    let deadline = Instant::now() + RELAY_DEADLINE;
    let status = loop {
        if let Some(status) = attach.child.try_wait().expect("poll remote connect") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let shim_alive = proc.exists();
    if status.is_none() {
        let _ = attach.child.kill();
        let _ = attach.child.wait();
    }
    if shim_alive && let Ok(pid) = pid.trim().parse() {
        let _ = nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    let output = String::from_utf8_lossy(&attach.terminal.finish()).into_owned();

    let status = status
        .unwrap_or_else(|| panic!("remote connect ran on past the relay error; output:\n{output}"));
    assert!(
        !status.success(),
        "remote connect succeeded; output:\n{output}"
    );
    assert!(
        output.contains("keyboard relay stopped") && output.contains("[remote] paste = false"),
        "the error names the relay and the off switch; output:\n{output}"
    );
    assert!(!shim_alive, "the attach was killed; output:\n{output}");
    assert_eq!(main_invocation_count(&log), 1, "no reconnect: {output}");
}

#[test]
fn an_attach_that_exits_while_input_is_written_ends_as_a_clean_detach() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let pair = remote_connect_pty();
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
    cmd.env("RIMZ_TEST_SSH_SLEEP_MS", "300");
    let mut attach = Attach::start(pair, cmd);
    attach.wait_for("the terminal never went raw", || {
        !read_tty_termios(&attach.tty_name)
            .local_flags
            .contains(LocalFlags::ICANON)
    });
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    while Instant::now() < deadline
        && attach
            .child
            .try_wait()
            .expect("poll remote connect")
            .is_none()
    {
        attach.terminal.write(b"typing through the detach ");
        std::thread::sleep(Duration::from_millis(1));
    }
    let output = attach.finish();

    assert!(
        !output.contains("keyboard relay"),
        "a clean detach reports no relay error; output:\n{output}"
    );
}
