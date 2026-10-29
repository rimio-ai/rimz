//! `rimz remote connect`: the SSH launcher and its reconnect supervisor.
//!
//! No real ssh or host anywhere: the print form needs no binary at all, and
//! the attach form drives the `ssh-trace` shim through `RIMZ_SSH_BIN`,
//! asserting the exact argv handed to ssh and scripting link drops via
//! `$RIMZ_TEST_SSH_PLAN`. Quoting precision lives in `remote/mod.rs` unit tests;
//! these prove the CLI surface end to end. Shared PTY and terminal-query
//! behavior lives in `common::ssh_trace` for the live mux suites too.

use std::io::{Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use nix::sys::socket::{self, AddressFamily, Backlog, SockFlag, SockType, SockaddrIn};
use nix::sys::termios::{self, InputFlags, LocalFlags, OutputFlags, Termios};
use portable_pty::{CommandBuilder, PtyPair};

use crate::common::ssh_trace::{
    Answers, FakeTerminal, is_control_check_invocation, is_main_invocation, is_master_invocation,
    is_path_preflight_invocation, is_probe_invocation, main_invocation_count,
    master_invocation_count, remote_connect_pty, remote_connect_pty_command, shim_invocations,
    ssh_shim,
};
use crate::common::{COMMAND_TIMEOUT, CommandTimeoutExt, Env};

#[path = "remote_ports.rs"]
mod ports;

fn stdout_line(out: &Output) -> String {
    assert!(
        out.status.success(),
        "expected success\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[test]
fn remote_setup_snippet_installs_or_names_latest_under_sh_and_zsh() {
    let env = Env::new();
    let bin = env.home_root.join("setup-bin");
    // ssh runs the snippet through the host login shell, which may be zsh.
    let ssh = crate::common::write_path_shim(
        &bin,
        "ssh",
        r#"for arg; do snippet=$arg; done; exec "$SETUP_TEST_SHELL" -c "$snippet""#,
    );
    // An empty release directory stands in for an unpublished release, and SETUP_TEST_SIGNAL
    // stands in for an ssh hangup mid-download. dd copies by read and write: cp and cat clone,
    // and cloning a just-written file on ZFS waits for a pool sync.
    crate::common::write_path_shim(
        &bin,
        "curl",
        r#"[ -z "$SETUP_TEST_SIGNAL" ] || kill -s "$SETUP_TEST_SIGNAL" "$PPID"; for arg; do url=$arg; done; src="$SETUP_TEST_RELEASE/${url##*/}"; [ -f "$src" ] || exit 22; dd if="$src" of="${url##*/}" 2>/dev/null"#,
    );
    crate::common::write_path_shim(
        &bin,
        "uname",
        r#"if [ "$1" = -s ]; then echo Linux; else echo x86_64; fi"#,
    );
    let published = env.home_root.join("release-published");
    let unpublished = env.home_root.join("release-unpublished");
    std::fs::create_dir_all(&unpublished).expect("create empty release");
    let packaged = Command::new("sh")
        .args([
            "-c",
            r#"set -e
mkdir -p "$1/rimz-x86_64-unknown-linux-gnu"
cd "$1"
printf '#!/bin/sh\necho rimz 0.0.0-test\n' > rimz-x86_64-unknown-linux-gnu/rimz
tar -czf rimz-x86_64-unknown-linux-gnu.tar.gz rimz-x86_64-unknown-linux-gnu
sha256sum rimz-x86_64-unknown-linux-gnu.tar.gz > SHA256SUMS"#,
            "sh",
        ])
        .arg(&published)
        .status()
        .expect("package fake release");
    assert!(packaged.success(), "package fake release: {packaged}");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let shells = ["sh", "zsh"].into_iter().filter(|shell| {
        Command::new(shell)
            .args(["-c", "true"])
            .status()
            .is_ok_and(|status| status.success())
    });
    let installed = env.home_root.join(".local/bin");
    for shell in shells {
        let setup = |release: &Path, signal: &str| {
            env.rimz()
                .args(["remote", "setup", "dev-box"])
                .env("RIMZ_SSH_BIN", &ssh)
                .env("SETUP_TEST_SHELL", shell)
                .env("SETUP_TEST_RELEASE", release)
                .env("SETUP_TEST_SIGNAL", signal)
                .env("PATH", &path)
                .bounded_output()
                .expect("run remote setup")
        };

        let out = setup(&published, "");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{shell}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("rimz 0.0.0-test installed on dev-box at ~/.local/bin/rimz"),
            "{shell}: {stdout}\n{stderr}"
        );
        assert_installed_alone(&installed, shell);
        let mode = std::fs::metadata(installed.join("rimz"))
            .expect("installed rimz")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755, "{shell}");

        let out = setup(&unpublished, "");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{shell}: {stderr}");
        assert!(
            stderr.contains("rerun with --latest to install the newest release"),
            "{shell}: {stderr}"
        );
        assert!(
            stderr.contains("failed with exit status: 22"),
            "{shell}: {stderr}"
        );
        assert_installed_alone(&installed, shell);

        // A signal that ends the shell must still remove the staging; each signal trap cleans up
        // itself, because zsh under `set -e` skips the EXIT trap when a signal trap calls exit.
        for (signal, code) in [("HUP", 129), ("INT", 130), ("TERM", 143)] {
            let out = setup(&published, signal);
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains(&format!("failed with exit status: {code}")),
                "{shell} {signal}: {stderr}"
            );
            assert_installed_alone(&installed, &format!("{shell} {signal}"));
        }
    }
}

/// The snippet stages inside the bin directory, so it must leave nothing there but `rimz`.
fn assert_installed_alone(bin: &Path, context: &str) {
    let entries: Vec<_> = std::fs::read_dir(bin)
        .expect("read installed bin dir")
        .map(|entry| entry.expect("bin dir entry").file_name())
        .collect();
    assert_eq!(entries, ["rimz"], "{context}");
}

#[test]
fn remote_list_empty_state_names_the_save_command() {
    let env = Env::new();
    let out = env
        .rimz()
        .args(["remote", "list"])
        .bounded_output()
        .unwrap();
    assert_eq!(
        stdout_line(&out),
        "no saved remotes; save one with: rimz remote add <name> [user@]host:<session-or-path>"
    );
}

#[test]
fn remote_print_distinguishes_auto_fallback_from_explicit_print() {
    let env = Env::new();
    for flag in [None, Some("--print"), Some("--no-attach")] {
        let mut command = env.rimz();
        command.args(["remote", "connect", "dev-box:query-engine"]);
        command.args(flag);
        let out = command.bounded_output().expect("print remote command");
        let line = stdout_line(&out);
        assert_eq!(line.lines().count(), 1);
        assert!(!line.contains("RIMZ_REMOTE_SUPERVISED"), "{line}");
        assert!(!line.contains("RIMZ_OUTER_SCROLL_BRACKET"), "{line}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            stderr,
            if flag.is_none() {
                "rimz: no terminal here, so nothing was connected; printed the ssh command instead (same as --print). Pass --attach to connect anyway.\n"
            } else {
                ""
            }
        );
    }
}

#[test]
fn remote_web_print_refusal_names_the_cause() {
    let env = Env::new();
    for (flag, message) in [
        (
            None,
            "--web needs a terminal to run the tunnel and has no ssh command to print; pass --attach to run it anyway",
        ),
        (
            Some("--print"),
            "--web opens a browser tunnel and has no ssh command to print; drop --print",
        ),
    ] {
        let mut command = env.rimz();
        command.args(["remote", "connect", "dev-box:query-engine", "--web"]);
        command.args(flag);
        let out = command.bounded_output().expect("web print refusal");
        assert_eq!(out.status.code(), Some(1));
        assert!(out.stdout.is_empty());
        assert!(String::from_utf8_lossy(&out.stderr).contains(message));
    }
}

#[test]
fn remote_connect_help_describes_printing_without_opening_a_room() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["remote", "connect", "--help"])
        .bounded_output()
        .expect("remote connect help");
    let help = stdout_line(&output);
    assert!(
        help.contains("Open the remote room in the local browser"),
        "{help}"
    );
    assert!(help.contains("8300..8399"), "{help}");
    assert!(
        help.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .contains("session name"),
        "{help}"
    );
    assert!(
        help.contains("Print the SSH command instead of connecting; opens nothing"),
        "{help}"
    );
    assert!(
        help.contains("Connect to the remote room instead of only printing the SSH command"),
        "{help}"
    );
    assert!(help.contains("Alias for `--no-attach`"), "{help}");
    assert!(!help.contains("Open the room, then print"), "{help}");
    assert!(!help.contains("Enter the room after opening it"), "{help}");

    for args in [
        vec!["--help"],
        vec!["start", "--help"],
        vec!["attach", "--help"],
    ] {
        let output = env.rimz().args(args).bounded_output().expect("local help");
        let help = stdout_line(&output)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            help.contains("Open the room, then print the attach command instead of entering it"),
            "{help}"
        );
        assert!(
            help.contains("Enter the room after opening it, where RimZ would otherwise only print the attach command"),
            "{help}"
        );
    }
}

fn write_infocmp_shim(path: &Path) {
    std::fs::write(path, "#!/bin/sh\nprintf 'CANNED,'\n").expect("write infocmp shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let permissions = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod infocmp shim");
    }
}

enum InfocmpFixture {
    Missing,
    Copy,
}

fn remote_connect_command(env: &Env, log: &Path) -> Command {
    let mut cmd = env.rimz();
    cmd.args(["remote", "connect", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", log)
        .env("RIMZ_REMOTE_INTERNET_PROBE", "0")
        .env("RIMZ_REMOTE_DIAL_MS", "0")
        .env("RIMZ_REMOTE_PROBE_MS", "0")
        .env("RIMZ_REMOTE_REACHABLE_RETRY_MS", "1")
        .env("RIMZ_REMOTE_MIN_DISPLAY_MS", "0")
        .env("TERM", "xterm-256color");
    cmd
}

fn remote_path_connect_command(env: &Env, log: &Path) -> Command {
    let mut cmd = env.rimz();
    cmd.args([
        "remote",
        "connect",
        "dev-box:workspace/query-engine",
        "--attach",
    ])
    .env("RIMZ_SSH_BIN", ssh_shim())
    .env("RIMZ_TEST_SSH_LOG", log)
    .env("RIMZ_REMOTE_DIAL_MS", "0")
    .env("RIMZ_REMOTE_PROBE_MS", "0")
    .env("RIMZ_REMOTE_REACHABLE_RETRY_MS", "1")
    .env("RIMZ_REMOTE_MIN_DISPLAY_MS", "0")
    .env("TERM", "xterm-256color");
    cmd
}

fn put_pty_in_raw_mode(pair: &PtyPair) {
    let mut cmd = CommandBuilder::new("stty");
    cmd.args(["raw", "-echo"]);
    let status = pair
        .slave
        .spawn_command(cmd)
        .expect("spawn stty raw")
        .wait()
        .expect("wait stty raw");
    assert!(status.success(), "stty raw failed: {status:?}");
}

fn read_pty_termios(pair: &PtyPair) -> Termios {
    read_tty_termios(&pair.master.tty_name().expect("remote connect pty name"))
}

fn read_tty_termios(path: &Path) -> Termios {
    let tty = std::fs::File::open(path).expect("open remote connect pty");
    termios::tcgetattr(&tty).expect("read remote connect pty state")
}

fn run_pty_command(pair: PtyPair, cmd: CommandBuilder) -> (String, Termios) {
    let mut child = pair.slave.spawn_command(cmd).expect("spawn remote connect");
    let tty_name = pair.master.tty_name().expect("remote connect pty name");
    drop(pair.slave);
    let terminal = FakeTerminal::new(pair.master, Duration::ZERO, Answers::StatusOnly);
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll remote connect") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let settings = read_tty_termios(&tty_name);
    let output = String::from_utf8_lossy(&terminal.finish()).into_owned();
    let status = status.unwrap_or_else(|| panic!("remote connect timed out; output:\n{output}"));
    assert!(
        status.success(),
        "remote connect failed with {status:?}; output:\n{output}"
    );
    (output, settings)
}

fn assert_shell_tty(settings: &Termios) {
    assert!(
        settings.input_flags.contains(InputFlags::ICRNL),
        "ICRNL missing: {settings:?}"
    );
    assert!(
        settings
            .output_flags
            .contains(OutputFlags::OPOST | OutputFlags::ONLCR),
        "OPOST/ONLCR missing: {settings:?}"
    );
    assert!(
        settings
            .local_flags
            .contains(LocalFlags::ICANON | LocalFlags::ISIG | LocalFlags::ECHO),
        "ICANON/ISIG/ECHO missing: {settings:?}"
    );
}

fn run_exec_with_term(colorterm: Option<&str>, infocmp: InfocmpFixture) -> Vec<String> {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let mut cmd = remote_connect_command(&env, &log);
    cmd.env("TERM", "alacritty");
    match colorterm {
        Some(value) => {
            cmd.env("COLORTERM", value);
        }
        None => {
            cmd.env_remove("COLORTERM");
        }
    }
    match infocmp {
        InfocmpFixture::Missing => {
            cmd.env("RIMZ_INFOCMP_BIN", env.project_root.join("missing-infocmp"));
        }
        InfocmpFixture::Copy => {
            let infocmp = env.project_root.join("infocmp-shim");
            write_infocmp_shim(&infocmp);
            cmd.env("RIMZ_INFOCMP_BIN", infocmp);
        }
    }
    let out = cmd
        .bounded_output()
        .expect("run rimz remote connect --attach");
    assert!(
        out.status.success(),
        "shim exits 0 -> clean exit\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log);
    let master_index = invocations
        .iter()
        .position(|argv| is_master_invocation(argv))
        .expect("initial master invocation");
    let main_index = invocations
        .iter()
        .position(|argv| is_main_invocation(argv))
        .expect("main ssh invocation");
    assert!(
        master_index < main_index,
        "the confirmed master precedes the main attach: {invocations:?}"
    );
    assert_eq!(
        invocations
            .iter()
            .filter(|argv| is_main_invocation(argv))
            .count(),
        1,
        "one main ssh run: {invocations:?}"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("attaching to"),
        "the panel owns supervised connection presentation"
    );
    invocations[main_index].clone()
}

fn snippet(argv: &[String]) -> &str {
    argv.last().expect("snippet")
}

fn remote_dispatch_env() -> Env {
    use std::os::unix::fs::PermissionsExt as _;

    let env = Env::new();
    let bin = env.home_root.join(".cargo/bin");
    std::fs::create_dir_all(&bin).expect("mkdir fake remote bin");
    let rimz = bin.join("rimz");
    std::fs::write(&rimz, "#!/bin/sh\nprintf '%s\\0' \"$@\"\n").expect("write remote argv printer");
    std::fs::set_permissions(&rimz, std::fs::Permissions::from_mode(0o755))
        .expect("chmod remote argv printer");
    for name in [".agents", "bare", "nested/project", "a'\"$HOME`pwd`$(pwd)"] {
        std::fs::create_dir_all(env.home_root.join(name)).expect("mkdir remote workspace");
    }
    // A directory in the SSH shell's cwd must not satisfy a HOME-relative target.
    std::fs::create_dir_all(env.project_root.join("absent")).expect("mkdir cwd decoy");
    env
}

fn run_remote_snippet(env: &Env, command: &str) -> Output {
    env.rimz_at(Path::new("/bin/sh"))
        .args(["-c", command])
        .env("PATH", "/usr/bin:/bin")
        .bounded_output()
        .expect("execute remote snippet")
}

fn remote_argv(out: &Output) -> Vec<String> {
    stdout_line(out);
    String::from_utf8(out.stdout.clone())
        .expect("remote argv is UTF-8")
        .split_terminator('\0')
        .map(str::to_owned)
        .collect()
}

fn printed_remote_snippet(env: &Env, target: &str, flags: &[&str]) -> String {
    let out = env
        .rimz()
        .args(["remote", "connect", target, "--print"])
        .args(flags)
        .env("TERM", "xterm-256color")
        .bounded_output()
        .expect("print remote connect");
    let argv = shlex::split(&stdout_line(&out)).expect("parse printed SSH argv");
    snippet(&argv).to_owned()
}

fn remote_dispatch_cases() -> [(&'static str, &'static str, bool); 10] {
    [
        (".agents", ".agents", true),
        ("bare", "bare", true),
        ("./bare", "./bare", true),
        ("~/.agents", ".agents", true),
        ("nested/project", "nested/project", true),
        ("session:bare", "bare", false),
        ("absent", "absent", false),
        ("a'\"$HOME`pwd`$(pwd)", "a'\"$HOME`pwd`$(pwd)", true),
        (
            "session:a'\"$HOME`pwd`$(pwd)",
            "a'\"$HOME`pwd`$(pwd)",
            false,
        ),
        (
            "missing'\"$HOME`pwd`$(pwd)",
            "missing'\"$HOME`pwd`$(pwd)",
            false,
        ),
    ]
}

#[test]
fn remote_print_and_saved_alias_dispatch_from_home_with_mux_and_no_resume() {
    let local = Env::new();
    let remote = remote_dispatch_env();
    std::fs::create_dir_all(local.home_root.join("absent")).expect("mkdir local decoy");
    for mux in ["tmux", "zellij"] {
        for (suffix, value, is_path) in remote_dispatch_cases() {
            let target = format!("dev-box:{suffix}");
            let expected_target = if is_path {
                remote.home_root.join(value).display().to_string()
            } else {
                value.to_owned()
            };
            let expected = vec![
                if is_path { "start" } else { "attach" },
                "--attach",
                "--no-resume",
                "--mux",
                mux,
                "--",
                &expected_target,
            ];
            let raw = printed_remote_snippet(&local, &target, &["--mux", mux, "--no-resume"]);
            assert_eq!(
                remote_argv(&run_remote_snippet(&remote, &raw)),
                expected,
                "{target}"
            );

            let alias = format!("case-{mux}");
            let verb = if suffix == ".agents" { "add" } else { "update" };
            let saved = local
                .rimz()
                .args(["remote", verb, &alias, &target, "--mux", mux, "--no-resume"])
                .bounded_output()
                .expect("save remote alias");
            stdout_line(&saved);
            let saved = printed_remote_snippet(&local, &alias, &[]);
            assert_eq!(
                remote_argv(&run_remote_snippet(&remote, &saved)),
                expected,
                "{alias}: {target}"
            );
        }
    }
}

#[test]
fn remote_web_and_probe_snippets_share_home_directory_and_session_dispatch() {
    use rimz::remote::RemoteTarget;
    use rimz::remote::link::probe_stream_spec;
    use rimz::remote::web::{WebPrepOptions, web_prep_spec};

    let env = remote_dispatch_env();
    for (suffix, value, is_path) in remote_dispatch_cases() {
        let target = RemoteTarget::parse(&format!("dev-box:{suffix}")).expect("remote target");
        let expected_target = if is_path {
            env.home_root.join(value).display().to_string()
        } else {
            value.to_owned()
        };
        let web = web_prep_spec(&target, WebPrepOptions::default(), None);
        assert_eq!(
            remote_argv(&run_remote_snippet(&env, snippet(&web.args))),
            [
                "web",
                "open",
                "--print",
                "--json",
                if is_path { "--" } else { "--session" },
                &expected_target
            ],
            "web: {suffix}"
        );
        let probe = probe_stream_spec(&target, &env.home_root.join("control.sock"));
        assert_eq!(
            remote_argv(&run_remote_snippet(&env, snippet(&probe.args))),
            [
                "remote",
                "link-stats",
                "ingest",
                if is_path { "--dir" } else { "--session" },
                &expected_target
            ],
            "probe: {suffix}"
        );
    }
}

#[test]
fn remote_explicit_missing_paths_refuse_instead_of_attaching() {
    use rimz::remote::RemoteTarget;
    use rimz::remote::web::{WebPrepOptions, web_prep_spec};

    let env = remote_dispatch_env();
    for suffix in ["./absent", "~/absent", "./missing'\"$HOME`pwd`$(pwd)"] {
        let target = format!("dev-box:{suffix}");
        let saved = env
            .rimz()
            .args([
                "remote",
                if suffix == "./absent" {
                    "add"
                } else {
                    "update"
                },
                "missing",
                &target,
            ])
            .bounded_output()
            .expect("save missing path alias");
        stdout_line(&saved);
        let web = web_prep_spec(
            &RemoteTarget::parse(&target).expect("remote target"),
            WebPrepOptions::default(),
            None,
        );
        for command in [
            printed_remote_snippet(&env, &target, &[]),
            printed_remote_snippet(&env, "missing", &[]),
            snippet(&web.args).to_owned(),
        ] {
            let out = run_remote_snippet(&env, &command);
            assert_eq!(out.status.code(), Some(67), "{target}: {out:?}");
            assert!(out.stdout.is_empty(), "must not execute rimz: {out:?}");
            assert!(String::from_utf8_lossy(&out.stderr).contains("remote path does not exist"));
        }
    }
}

fn write_link_notify_command_config(env: &Env) {
    let dir = env.rimz_home();
    std::fs::create_dir_all(&dir).expect("mkdir rimz config dir");
    std::fs::write(
        dir.join("config.toml"),
        "[notifications]\ncommand = '''printf '%s|%s|%s\\n' \"$RIMZ_NOTIFY_KIND\" \"$RIMZ_NOTIFY_TITLE\" \"$RIMZ_NOTIFY_BODY\" >> \"$RIMZ_NOTIFY_TEST_LOG\"'''\n",
    )
    .expect("write notify config");
}

fn wait_for_notify_log(path: &Path, needles: &[&str]) -> String {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if needles.iter().all(|needle| text.contains(needle)) {
            return text;
        }
        assert!(
            Instant::now() < deadline,
            "notify log did not contain {needles:?}; saw:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn tunnel_invocation_count(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.split('\t').any(|arg| arg == "-L"))
        .count()
}

fn tunnel_forward_ports(log: &Path) -> Vec<u16> {
    shim_invocations(log)
        .into_iter()
        .filter_map(|argv| {
            let forwarding = argv.windows(2).find(|args| args[0] == "-L")?.get(1)?;
            forwarding.split(':').nth(1)?.parse().ok()
        })
        .collect()
}

fn web_prep_invocation_count(log: &Path) -> usize {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.contains("rimz web open"))
        .count()
}

/// Keep the probe below the ephemeral range's low end so a port-0 bind cannot
/// take it between this probe and the child's auto-forward availability check.
/// A host whose range starts at or below 20000 leaves no such port, and the
/// scan then runs inside the range rather than failing the test outright.
fn available_forward_probe_port() -> u16 {
    let ceiling = std::fs::read_to_string("/proc/sys/net/ipv4/ip_local_port_range")
        .ok()
        .and_then(|range| range.split_whitespace().next()?.parse::<u16>().ok())
        .filter(|low| *low > 20000)
        .unwrap_or(32768);
    let offset = std::process::id() % u32::from(ceiling - 20000);
    let start = 20000 + u16::try_from(offset).expect("probe offset fits below the ceiling");
    (start..ceiling)
        .chain(20000..start)
        .find(|port| TcpListener::bind(("127.0.0.1", *port)).is_ok())
        .expect("free auto-forward probe port below the ephemeral range")
}

fn relay_url(stdout: &str) -> (u16, &str) {
    let line = stdout.lines().next().unwrap_or_default();
    let url = url::Url::parse(line)
        .unwrap_or_else(|error| panic!("invalid relay URL: {error}; stdout:\n{stdout}"));
    let port = url
        .port()
        .unwrap_or_else(|| panic!("relay URL has no port; stdout:\n{stdout}"));
    assert_eq!(
        line,
        format!("http://127.0.0.1:{port}/?room=rimz-project-a1b2c3"),
        "unexpected relay URL; stdout:\n{stdout}"
    );
    (port, line)
}

fn reserved_ssh_endpoint(env: &Env) -> (PathBuf, OwnedFd) {
    let path = env.project_root.join("ssh-config.txt");
    let reservation = socket::socket(
        AddressFamily::Inet,
        SockType::Stream,
        SockFlag::empty(),
        None,
    )
    .expect("create dial socket");
    nix::fcntl::fcntl(
        &reservation,
        nix::fcntl::FcntlArg::F_SETFD(nix::fcntl::FdFlag::FD_CLOEXEC),
    )
    .expect("keep dial socket out of child processes");
    socket::bind(reservation.as_raw_fd(), &SockaddrIn::new(127, 0, 0, 1, 0))
        .expect("reserve dial port without listening");
    let address: SockaddrIn = socket::getsockname(reservation.as_raw_fd()).expect("dial address");
    std::fs::write(
        &path,
        format!("hostname 127.0.0.1\nport {}\n", address.port()),
    )
    .expect("write ssh config fixture");
    (path, reservation)
}

fn http_204_probe() -> (String, Arc<AtomicBool>, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind HTTP probe");
    listener
        .set_nonblocking(true)
        .expect("make HTTP probe nonblocking");
    let address = listener.local_addr().expect("HTTP probe address");
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let join = std::thread::spawn(move || {
        while !thread_stop.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let mut request = [0; 1024];
                    let _ = stream.read(&mut request);
                    let _ =
                        stream.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n");
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return,
            }
        }
    });
    (format!("http://{address}/generate_204"), stop, join)
}

fn remote_web_command(env: &Env, log: &Path) -> Command {
    let mut cmd = remote_connect_command(env, log);
    cmd.arg("--web");
    cmd
}

fn write_browser_shim(env: &Env) -> (PathBuf, PathBuf) {
    let bin = env.project_root.join("browser-bin");
    let log = env.project_root.join("browser.log");
    std::fs::create_dir_all(&bin).expect("create browser bin");
    let opener = bin.join("xdg-open");
    std::fs::write(
        &opener,
        "#!/bin/sh\nprintf '%s\\n' \"$1\" > \"$RIMZ_TEST_BROWSER_LOG\"\n",
    )
    .expect("write browser shim");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o755))
            .expect("chmod browser shim");
    }
    (bin, log)
}

fn wait_for_main_invocations(
    child: &mut std::process::Child,
    log: &Path,
    expected: usize,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    loop {
        let count = main_invocation_count(log);
        if count >= expected {
            return;
        }
        if let Some(status) = child.try_wait().expect("poll remote connect") {
            let count = main_invocation_count(log);
            if count >= expected {
                return;
            }
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let trace = std::fs::read_to_string(log).unwrap_or_default();
            panic!(
                "remote connect exited with {status} before {expected} main ssh invocations; \
                 saw {count}\ntrace:\n{trace}\nstderr:\n{stderr}"
            );
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            let trace = std::fs::read_to_string(log).unwrap_or_default();
            panic!(
                "expected {expected} main ssh invocations within {timeout:?}; saw {count}\ntrace:\n{trace}\nstderr:\n{stderr}"
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_tunnel_invocation(child: &mut std::process::Child, log: &Path) {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    while tunnel_invocation_count(log) == 0 {
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "web tunnel did not start; trace={:?}",
                shim_invocations(log)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_web_child<T>(
    child: &mut std::process::Child,
    stderr_path: &Path,
    log: &Path,
    event: &str,
    mut ready: impl FnMut(Option<std::process::ExitStatus>) -> Option<T>,
) -> T {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        let status = child.try_wait().expect("poll remote web");
        if let Some(value) = ready(status) {
            return value;
        }
        let failure = if let Some(status) = status {
            format!("remote web exited with {status} before {event}")
        } else if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            format!("remote web timed out waiting for {event} within {COMMAND_TIMEOUT:?}")
        } else {
            std::thread::sleep(Duration::from_millis(10));
            continue;
        };
        let trace = if log.exists() {
            shim_invocations(log)
        } else {
            Vec::new()
        };
        panic!(
            "{failure}\nstderr:\n{}\ntrace={trace:?}",
            std::fs::read_to_string(stderr_path).expect("read remote web stderr"),
        );
    }
}

fn wait_for_web_url(
    child: &mut std::process::Child,
    stdout_path: &Path,
    stderr_path: &Path,
    log: &Path,
) -> String {
    wait_for_web_child(child, stderr_path, log, "URL", |_| {
        let stdout = std::fs::read_to_string(stdout_path).expect("read remote web stdout");
        stdout
            .contains("/?room=rimz-project-a1b2c3\n")
            .then_some(stdout)
    })
}

fn wait_for_web_exit(
    mut child: std::process::Child,
    stdout_path: &Path,
    stderr_path: &Path,
    log: &Path,
) -> Output {
    let status = wait_for_web_child(&mut child, stderr_path, log, "exit", |status| status);
    Output {
        status,
        stdout: std::fs::read(stdout_path).expect("read remote web stdout"),
        stderr: std::fs::read(stderr_path).expect("read remote web stderr"),
    }
}

#[test]
fn link_stats_ingest_writes_the_runtime_sidecar_and_acks() {
    use std::io::{BufRead as _, Write as _};

    let env = Env::new();
    env.record(&env.project_root);
    let dir = env.project_root.to_string_lossy().into_owned();
    let proc_root = env.project_root.join("proc");
    let proc_net = proc_root.join("net");
    std::fs::create_dir_all(&proc_net).expect("create proc net fixture");
    let uid = nix::unistd::getuid().as_raw();
    let workspace_id = rimz::ids::WorkspaceId::from_project_root(&env.project_root).to_string();
    for (pid, inodes, pin) in [
        (101, vec![11, 16], Some(workspace_id.as_str())),
        (102, vec![12, 16], Some("another-room")),
        (103, vec![13], Some("")),
        (104, vec![14], None),
    ] {
        let process = proc_root.join(pid.to_string());
        std::fs::create_dir_all(process.join("fd")).expect("create owner fds");
        for inode in inodes {
            std::os::unix::fs::symlink(
                format!("socket:[{inode}]"),
                process.join("fd").join(inode.to_string()),
            )
            .expect("socket fd");
        }
        if let Some(pin) = pin {
            std::fs::write(
                process.join("environ"),
                format!("PATH=/bin\0RIMZ_WORKSPACE_ID={pin}\0"),
            )
            .expect("owner environment");
        }
    }
    let row = |local: &str, state: &str, row_uid: u32, inode: u64| {
        format!(
            "0: {local} 00000000:0000 {state} 00000000:00000000 00:00000000 00000000 {row_uid} 0 {inode}"
        )
    };
    std::fs::write(
        proc_net.join("tcp"),
        [
            row("0100007F:0BB8", "0A", uid, 11),
            row("0100007F:0FA0", "0A", uid, 12),
            row("0100007F:1388", "0A", uid, 13),
            row("0100007F:1770", "0A", uid, 14),
            row("0100007F:1B58", "0A", uid, 15),
            row("0100007F:0050", "0A", uid, 11),
            row("0100007F:2328", "0A", uid.saturating_add(1), 11),
            row("0200000A:2710", "0A", uid, 11),
        ]
        .join("\n"),
    )
    .expect("write tcp fixture");
    std::fs::write(
        proc_net.join("tcp6"),
        row("00000000000000000000000000000000:1F90", "0A", uid, 16),
    )
    .expect("write tcp6 fixture");
    let mut child = env
        .rimz()
        .args(["remote", "link-stats", "ingest", "--dir", &dir])
        .env("SSH_CONNECTION", "client-port server-port")
        .env("RIMZ_PROC_ROOT", &proc_root)
        .env("RIMZ_PORTS_SWEEP_MS", "0")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn link-stats ingest");
    let probe = serde_json::json!({
        "v": "rimz.link.v1",
        "seq": 7,
        "sent_at_ms": 1_000u64,
        "forwards": {
            "target": "dev", "host": "dev-box", "pid": 123,
            "started_at_ms": 100, "at_ms": 1000, "auto_forward": true,
            "forwards": [{"remote":3000,"local":3001,"state":"forwarded","since_ms":500}]
        },
        "stats": {
            "rtt_ms": 42,
            "miss_pct": 3,
            "window": 12
        }
    });
    let stdout = child.stdout.take().expect("stdout");
    let mut stdout = std::io::BufReader::new(stdout);
    let mut stdin = child.stdin.take().expect("stdin");
    writeln!(stdin, "{probe}").expect("write probe");
    let mut ack = String::new();
    stdout.read_line(&mut ack).expect("read link ack");
    let ack: serde_json::Value = serde_json::from_str(&ack).expect("ack json");
    assert_eq!(ack["v"], "rimz.link.v1");
    assert_eq!(ack["seq"], 7);
    assert_eq!(ack["ports"], serde_json::json!([3000, 8080]));
    assert_eq!(ack["ports_scope"], "room");

    let sidecar = rimz::remote::link::stats_path(&env.runtime_paths());
    let published: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
    assert_eq!(published["forwards"], probe["forwards"]);
    let mut legacy_probe = probe.clone();
    legacy_probe.as_object_mut().unwrap().remove("forwards");
    writeln!(stdin, "{legacy_probe}").unwrap();
    let mut legacy_ack = String::new();
    stdout.read_line(&mut legacy_ack).unwrap();
    let published: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&sidecar).unwrap()).unwrap();
    assert!(
        published.get("forwards").is_none(),
        "legacy probes clear the previous record"
    );

    std::fs::write(proc_root.join("101/environ"), b"PATH=/bin\0").unwrap();
    std::fs::write(
        proc_root.join("102/environ"),
        format!("RIMZ_WORKSPACE_ID={workspace_id}\0"),
    )
    .unwrap();
    for expected in [vec![3000, 8080], vec![8080], vec![4000, 8080]] {
        writeln!(stdin, "{probe}").expect("write next probe");
        let mut line = String::new();
        stdout.read_line(&mut line).expect("read next ack");
        let ack: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(
            ack["ports"],
            serde_json::json!(expected),
            "inode verdicts live only while listed"
        );
        assert_eq!(ack["ports_scope"], "room");
        std::fs::write(
            proc_net.join("tcp"),
            if expected.contains(&3000) {
                String::new()
            } else {
                [
                    row("0100007F:0BB8", "0A", uid, 11),
                    row("0100007F:0FA0", "0A", uid, 12),
                ]
                .join("\n")
            },
        )
        .unwrap();
    }

    let runtime = env.runtime_paths();
    let path = rimz::remote::link::stats_path(&runtime);
    let file: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).expect("stats file json");
    assert_eq!(file["v"], "rimz.link.v1");
    assert_eq!(file["client"], "client-port server-port");
    assert_eq!(file["stats"]["rtt_ms"], 42);
    assert_eq!(file["stats"]["miss_pct"], 3);

    drop(stdin);
    let out = child.wait_with_output().expect("wait link-stats ingest");
    assert!(
        out.status.success(),
        "ingest succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!path.exists(), "clean stream end removes its sidecar");
}

#[test]
fn link_stats_ingest_keeps_a_newer_publishers_sidecar() {
    let env = Env::new();
    env.record(&env.project_root);
    let runtime = env.runtime_paths();
    let path = rimz::remote::link::stats_path(&runtime);
    let seeded = rimz::remote::link::LinkStatsFile::new(
        1_000,
        "new-client-port new-server-port".to_owned(),
        rimz::remote::link::LinkStats::default(),
    );
    rimz::disk::atomic::write_temp_then_rename_cache(&path, &seeded)
        .expect("seed newer link stats");

    let dir = env.project_root.to_string_lossy().into_owned();
    let mut child = env
        .rimz()
        .args(["remote", "link-stats", "ingest", "--dir", &dir])
        .env("SSH_CONNECTION", "old-client-port old-server-port")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn link-stats ingest");
    drop(child.stdin.take());
    let out = child.wait_with_output().expect("wait link-stats ingest");
    assert!(
        out.status.success(),
        "ingest succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let remaining: rimz::remote::link::LinkStatsFile =
        serde_json::from_slice(&std::fs::read(path).expect("read seeded stats"))
            .expect("parse seeded stats");
    assert_eq!(remaining, seeded);
}

#[test]
fn supervised_connect_opens_new_remote_listener_forwards() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let port = available_forward_probe_port();
    let _busy = TcpListener::bind(("127.0.0.1", port)).expect("hold the remote port busy locally");
    let out = remote_connect_command(&env, &log)
        .env("RIMZ_REMOTE_PROBE_MS", "10")
        .env("RIMZ_TEST_PROBE_PORT", port.to_string())
        .env("RIMZ_TEST_WAIT_FOR_PROBE_MS", "500")
        .env("RIMZ_TEST_SSH_SLEEP_MS", "500")
        .bounded_output()
        .expect("run auto-forward connect");
    assert!(
        out.status.success(),
        "auto-forward connect succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let invocations = shim_invocations(&log);
    let forwards = invocations
        .iter()
        .filter(|argv| {
            argv.windows(2)
                .any(|args| args[0] == "-O" && args[1] == "forward")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        forwards.len(),
        1,
        "a busy local port is remapped, not skipped: {invocations:?}"
    );
    let forwarding = &forwards[0].windows(2).find(|args| args[0] == "-L").unwrap()[1];
    let local = forwarding
        .split(':')
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    assert_ne!(local, port, "the held local port cannot be reused");
    assert_eq!(forwarding, &format!("127.0.0.1:{local}:localhost:{port}"));
}

#[test]
fn supervised_connect_forwards_a_room_scoped_first_report() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let port = available_forward_probe_port();
    let out = remote_connect_command(&env, &log)
        .env("RIMZ_REMOTE_PROBE_MS", "10")
        .env("RIMZ_TEST_PROBE_PORT", port.to_string())
        .env("RIMZ_TEST_PROBE_PORTS_SCOPE", "room")
        .env("RIMZ_TEST_PROBE_SILENT_AFTER_ACKS", "1")
        .env("RIMZ_TEST_WAIT_FOR_PROBE_MS", "500")
        .env("RIMZ_TEST_SSH_SLEEP_MS", "500")
        .bounded_output()
        .expect("run room-scoped auto-forward connect");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        tunnel_forward_ports(&log),
        [port],
        "the first room report is not a baseline"
    );
}

#[test]
fn exec_downgrades_or_copies_terminal_at_the_cli_boundary() {
    let downgrade = run_exec_with_term(None, InfocmpFixture::Missing);
    assert!(
        snippet(&downgrade).contains("export TERM=xterm-256color; if test -d"),
        "{}",
        snippet(&downgrade)
    );

    let copy = run_exec_with_term(Some("truecolor"), InfocmpFixture::Copy);
    assert!(
        snippet(&copy).contains(
            "printf '%s\\n' 'CANNED,' | tic -x - >/dev/null 2>&1 && export TERM='alacritty'"
        ),
        "{}",
        snippet(&copy)
    );
    assert!(
        snippet(&copy).contains("export COLORTERM=truecolor;"),
        "{}",
        snippet(&copy)
    );
}

#[test]
fn reconnect_snippet_silences_terminfo_setup_in_login_shells() {
    let env = Env::new();
    let bin = env.home_root.join(".cargo/bin");
    std::fs::create_dir_all(&bin).unwrap();
    for (name, script) in [
        (
            "tic",
            "#!/bin/sh\nprintf 'tic stdout'\nprintf 'tic stderr' >&2\nexit \"$RIMZ_TEST_TIC_STATUS\"\n",
        ),
        (
            "rimz",
            "#!/bin/sh\nprintf '%s:%s:%s' \"$TERM\" \"$RIMZ_REMOTE_SUPERVISED\" \"$RIMZ_REMOTE_RECONNECT\"\n",
        ),
    ] {
        let path = bin.join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let plan = rimz::remote::SshAttachPlan::new(rimz::remote::SshAttachOptions {
        target: rimz::remote::RemoteTarget::parse("dev-box:session:query-engine").unwrap(),
        lineage: "test-lineage".into(),
        supervised: true,
        force_version: false,
        resume: true,
        mux: None,
        term: rimz::remote::TermPlan::Copy {
            name: "alacritty".into(),
            source: "CANNED,".into(),
        },
        truecolor: false,
        client_size: None,
    });
    let spec = plan.retry().plain();
    for shell in ["sh", "zsh"] {
        let Ok(shell) = which::which(shell) else {
            continue;
        };
        for (tic_status, term) in [("0", "alacritty"), ("1", "xterm-256color")] {
            let out = env
                .rimz_at(&shell)
                .args(["-c", snippet(&spec.args)])
                .env("PATH", "/usr/bin:/bin")
                .env("RIMZ_TEST_TIC_STATUS", tic_status)
                .bounded_output()
                .unwrap();
            assert!(out.status.success(), "{shell:?}: {out:?}");
            assert_eq!(
                String::from_utf8_lossy(&out.stdout),
                format!("{term}:1:1"),
                "{shell:?}: tic output must not precede the host"
            );
            assert!(out.stderr.is_empty(), "{shell:?}: {out:?}");
        }
    }
}

#[test]
fn one_shot_connect_repairs_a_raw_tty_and_brackets_the_client() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let pair = remote_connect_pty();
    put_pty_in_raw_mode(&pair);
    let damaged = read_pty_termios(&pair);
    assert!(rimz::remote::tty::termios_damaged(
        damaged.input_flags,
        damaged.output_flags,
        damaged.local_flags
    ));

    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.arg("--no-reconnect");
    cmd.env("RIMZ_TEST_SSH_STDERR", "child output");
    let (output, settings) = run_pty_command(pair, cmd);

    assert_shell_tty(&settings);
    let save = output
        .find("\x1b[?1007s")
        .expect("attach saves alternate scroll");
    let disable = output
        .find("\x1b[?1007l")
        .expect("attach disables alternate scroll");
    let child = output
        .find("child output")
        .expect("attach child writes while alternate scroll is disabled");
    let restore = output
        .rfind("\x1b[?1007r")
        .expect("attach restores alternate scroll");
    assert!(
        save < disable && disable < child && child < restore,
        "alternate scroll brackets the client lifetime: {output:?}",
    );
    assert!(
        !output.contains("attaching to"),
        "one-shot attach leaves presentation to ssh: {output}"
    );
}

#[test]
fn one_shot_connect_preserves_the_ssh_exit_code_without_dumping_argv() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    std::fs::write(&plan, "23\n").expect("write ssh plan");

    let out = remote_connect_command(&env, &log)
        .arg("--no-reconnect")
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .bounded_output()
        .expect("run one-shot attach");

    assert_eq!(out.status.code(), Some(23));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("attach command") && !stderr.contains("ServerAliveInterval"),
        "the child owns its failure presentation: {stderr}",
    );
    assert!(
        snippet(&shim_invocations(&log)[0]).contains("RIMZ_OUTER_SCROLL_BRACKET=1"),
        "the outer SSH launcher suppresses a nested remote bracket",
    );
    assert!(
        !snippet(&shim_invocations(&log)[0]).contains("RIMZ_REMOTE_SUPERVISED"),
        "one-shot attaches have no sentinel consumer",
    );
    assert!(
        !snippet(&shim_invocations(&log)[0]).contains("--outer-scroll-bracket"),
        "older remote CLIs must not receive a new argument",
    );
}

#[cfg(unix)]
#[test]
fn one_shot_connect_mirrors_the_ssh_clients_suspend() {
    use nix::sys::signal::{Signal, killpg};
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
    use nix::unistd::Pid;
    use std::os::unix::process::CommandExt as _;

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let mut cmd = remote_connect_command(&env, &log);
    cmd.arg("--no-reconnect");
    cmd.env("RIMZ_TEST_SSH_SUSPEND", "1");
    cmd.process_group(0);

    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn remote connect");
    let pid = Pid::from_raw(i32::try_from(child.id()).expect("pid fits i32"));

    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG | WaitPidFlag::WUNTRACED))
            .expect("poll launcher job control")
        {
            WaitStatus::Stopped(stopped, Signal::SIGTSTP) if stopped == pid => break,
            WaitStatus::StillAlive if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            status => panic!("launcher did not mirror the child stop: {status:?}"),
        }
    }

    killpg(pid, Signal::SIGCONT).expect("foreground the attach process group");
    let status = child.wait().expect("wait resumed remote connect");
    assert!(status.success(), "resumed attach failed with {status}");
}

#[test]
fn supervised_missing_remote_path_fails_before_attach() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let preflight_plan = env.project_root.join("ssh-path-preflight.plan");
    std::fs::write(&preflight_plan, "1\n").expect("write path preflight plan");

    let out = remote_path_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PATH_PREFLIGHT_PLAN", &preflight_plan)
        .bounded_output()
        .expect("run missing-path remote connect");

    assert_eq!(
        out.status.code(),
        Some(67),
        "missing path must preserve its code"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("remote path does not exist on dev-box: workspace/query-engine"),
        "{stderr}"
    );
    assert!(stderr.contains("rimz remote list"), "{stderr}");
    let invocations = shim_invocations(&log);
    assert_eq!(
        invocations
            .iter()
            .filter(|argv| is_path_preflight_invocation(argv))
            .count(),
        1,
        "one path preflight: {invocations:?}"
    );
    assert_eq!(
        main_invocation_count(&log),
        0,
        "the tty attach must not run: {invocations:?}"
    );
}

#[test]
fn supervised_existing_remote_path_preflights_before_attach() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");

    let out = remote_path_connect_command(&env, &log)
        .bounded_output()
        .expect("run existing-path remote connect");

    assert!(
        out.status.success(),
        "existing path reaches attach\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log);
    let preflight_index = invocations
        .iter()
        .position(|argv| is_path_preflight_invocation(argv))
        .expect("path preflight invocation");
    let attach_index = invocations
        .iter()
        .position(|argv| is_main_invocation(argv))
        .expect("main attach invocation");
    assert!(
        preflight_index < attach_index,
        "preflight precedes attach: {invocations:?}"
    );
}

#[test]
fn supervised_auto_target_skips_path_preflight() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");

    let out = remote_connect_command(&env, &log)
        .bounded_output()
        .expect("run automatic remote connect");

    assert!(out.status.success(), "automatic attach succeeds");
    assert!(
        !shim_invocations(&log)
            .iter()
            .any(|argv| is_path_preflight_invocation(argv)),
        "automatic targets retain their session fallback without a directory preflight"
    );
}

#[test]
fn supervised_initial_auth_failure_falls_back_to_one_interactive_attach() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let master_plan = env.project_root.join("ssh-master.plan");
    std::fs::write(&plan, "2\n").expect("write main plan");
    std::fs::write(&master_plan, "255\n").expect("write master plan");

    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_MASTER_PLAN", &master_plan)
        .env(
            "RIMZ_TEST_SSH_MASTER_STDERR",
            "Permission denied (publickey).\n",
        )
        .bounded_output()
        .expect("run supervised remote connect");

    assert!(
        !out.status.success(),
        "the foreground failure remains fatal"
    );
    assert_eq!(master_invocation_count(&log), 1);
    assert_eq!(main_invocation_count(&log), 1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("connect to dev-box failed — Permission denied (publickey)."),
        "{stderr}"
    );
    assert!(
        stderr.contains("ssh to dev-box exited with status 2; not reconnecting"),
        "{stderr}"
    );
}

#[test]
fn established_interactive_fallback_failure_stays_fatal() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let master_plan = env.project_root.join("ssh-master.plan");
    std::fs::write(&plan, "1\n").expect("write main plan");
    std::fs::write(&master_plan, "255\n").expect("write master plan");

    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_MASTER_PLAN", &master_plan)
        .env(
            "RIMZ_TEST_SSH_MASTER_STDERR",
            "Permission denied (publickey).\n",
        )
        .env("RIMZ_TEST_SSH_SLEEP_MS", "80")
        .env("RIMZ_REMOTE_GATETIME_MS", "20")
        .bounded_output()
        .expect("run established interactive fallback");

    assert!(!out.status.success(), "the foreground failure stays fatal");
    assert_eq!(master_invocation_count(&log), 1);
    assert_eq!(main_invocation_count(&log), 1);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(
            "rimz on dev-box exited with status 1 before the room opened; not reconnecting"
        ),
        "{stderr}"
    );
    assert!(!stderr.contains("link to dev-box lost"), "{stderr}");
}

#[test]
fn host_text_lands_in_scrollback_without_holding_for_input() {
    const REMOTE_DETAIL: &str =
        "REMOTE DETAIL: room startup failed\nREMOTE DETAIL: run rimz doctor\n";

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    std::fs::write(&plan, "1\n").expect("write ssh plan");

    let pair = remote_connect_pty();
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_PLAN", &plan);
    cmd.env("RIMZ_TEST_SSH_STDOUT", REMOTE_DETAIL);
    cmd.env("RIMZ_REMOTE_GATETIME_MS", "5000");
    cmd.env("RIMZ_REMOTE_GRACE_MS", "0");
    cmd.env("RIMZ_REMOTE_MIN_DISPLAY_MS", "100");

    let mut child = pair.slave.spawn_command(cmd).expect("spawn remote connect");
    drop(pair.slave);
    let output = Arc::new(Mutex::new(Vec::new()));
    let reader_output = Arc::clone(&output);
    let mut reader = pair.master.try_clone_reader().expect("clone pty reader");
    let reader_thread = std::thread::spawn(move || {
        let mut chunk = [0_u8; 1024];
        while let Ok(count) = reader.read(&mut chunk) {
            if count == 0 {
                break;
            }
            reader_output
                .lock()
                .expect("lock pty output")
                .extend_from_slice(&chunk[..count]);
        }
    });
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll remote connect") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(pair.master);
    reader_thread.join().expect("join pty reader");
    let final_output =
        String::from_utf8_lossy(&output.lock().expect("lock pty output")).into_owned();
    let status = status.unwrap_or_else(|| {
        panic!("host text must exit without waiting for input: {final_output:?}")
    });
    assert!(!status.success(), "fatal attach exits nonzero");

    let detail = final_output.find("REMOTE DETAIL").expect("remote detail");
    let leave = final_output
        .find("\x1b[?1049l")
        .expect("leave alternate screen");
    let report = final_output
        .rfind("not reconnecting")
        .expect("fatal scrollback report");
    assert!(
        leave < detail && detail < report,
        "the panel leaves before host text: {final_output:?}"
    );
    let mut parser = vt100::Parser::new(24.try_into().unwrap(), 100.try_into().unwrap(), 100);
    parser.process(final_output.as_bytes());
    assert!(!parser.screen().alternate_screen());
    let main = parser.screen().contents();
    let separator = main
        .find("── dev-box:query-engine · ")
        .expect("separator on main screen");
    assert!(
        separator
            < main
                .find("REMOTE DETAIL: room startup failed")
                .expect("host text on main screen"),
        "{main}"
    );
    assert!(
        main.lines()
            .any(|line| line.starts_with("REMOTE DETAIL: run rimz doctor")),
        "{main}"
    );
}

#[test]
fn supervised_silent_handoff_strips_the_sentinel_and_only_marks_a_colored_panel() {
    for no_color in [false, true] {
        let env = Env::new();
        let log = env.project_root.join("ssh-trace.log");
        let mut cmd = remote_connect_pty_command(&env, &log);
        cmd.env("RIMZ_REMOTE_GRACE_MS", "0");
        cmd.env("RIMZ_REMOTE_MIN_DISPLAY_MS", "100");
        cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
        cmd.env(
            "RIMZ_TEST_SSH_STDOUT",
            format!(
                "\x1b[?1004h{}\x1b[?1049hROOM\x1b[?1049l",
                rimz::remote::tty::ROOM_OPEN_SENTINEL
            ),
        );
        if no_color {
            cmd.env("NO_COLOR", "1");
        } else {
            cmd.env_remove("NO_COLOR");
        }
        let (output, _) = run_pty_command(remote_connect_pty(), cmd);
        assert!(
            !output.contains("rimz-room-open"),
            "sentinel leaked: {output:?}"
        );
        assert!(
            !output.contains("── dev-box"),
            "silent connect has no separator: {output:?}"
        );
        assert_eq!(
            output.contains("\x1b[32m✓\x1b[39m"),
            !no_color,
            "{output:?}"
        );
        let room = output.find("ROOM").unwrap();
        assert!(
            !output[..room].contains("\x1b[?1049l"),
            "panel stays until room paint: {output:?}"
        );
        assert_eq!(
            output.matches("\x1b[?1049l").count(),
            1,
            "only the mux leaves the alternate screen: {output:?}"
        );
    }
}

#[test]
fn supervised_held_panel_replays_logs_on_the_main_screen() {
    assert_supervised_logs_survive(
        &format!("{}\x1b[?1049hROOM", rimz::remote::tty::ROOM_OPEN_SENTINEL),
        false,
    );
}

#[test]
fn supervised_stepped_off_panel_defers_logs_until_session_end() {
    assert_supervised_logs_survive("host text\r\n", true);
}

fn assert_supervised_logs_survive(payload: &str, raw_tty: bool) {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_REMOTE_GRACE_MS", "0");
    cmd.env("RIMZ_REMOTE_MIN_DISPLAY_MS", "100");
    cmd.env("RUST_LOG", "trace");
    cmd.env("RIMZ_TEST_SSH_STDOUT", payload);
    if raw_tty {
        cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
    }
    let (output, settings) = run_pty_command(remote_connect_pty(), cmd);
    assert_shell_tty(&settings);

    let mut parser = vt100::Parser::new(24.try_into().unwrap(), 100.try_into().unwrap(), 100);
    parser.process(output.as_bytes());
    assert!(!parser.screen().alternate_screen());
    let main = parser.screen().contents();
    parser.screen_mut().set_scrollback(24);
    let mut rows = parser
        .screen()
        .rows(0, 100)
        .take(parser.screen().scrollback())
        .collect::<Vec<_>>();
    rows.extend(main.lines().map(str::to_owned));
    let logs = rows
        .iter()
        .filter(|line| line.contains("mio::poll"))
        .collect::<Vec<_>>();
    assert!(!logs.is_empty(), "captured logs survive: {rows:?}");
    assert!(
        logs.iter().all(|line| line.starts_with("20")),
        "each captured log starts at column 0: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|line| line == "rimz: detached from dev-box"),
        "the end line starts at column 0: {rows:?}"
    );
    let first_log = output.find("mio::poll").expect("captured trace log");
    let reset = output
        .rfind(rimz::remote::tty::EMULATOR_RESET)
        .expect("session emulator reset");
    assert!(reset < first_log, "logs follow emulator reset: {output:?}");
    if raw_tty {
        assert!(
            rows.iter()
                .any(|line| line.starts_with("── dev-box:query-engine · ")),
            "the separator starts at column 0: {rows:?}"
        );
        assert!(
            output.find("host text").unwrap() < first_log,
            "logs stay captured throughout host output: {output:?}"
        );
    } else {
        let leave = output.rfind("\x1b[?1049l").expect("final screen leave");
        assert!(
            leave < first_log,
            "no log precedes the final screen leave: {output:?}"
        );
    }
}

#[test]
fn supervised_end_line_preserves_the_command_row_with_and_without_a_panel() {
    for panel in [false, true] {
        let env = Env::new();
        let log = env.project_root.join("ssh-trace.log");
        let pair = remote_connect_pty();
        let mut tty = std::fs::OpenOptions::new()
            .write(true)
            .open(pair.master.tty_name().unwrap())
            .unwrap();
        tty.write_all(b"$ rimz remote connect dev\r\n").unwrap();
        drop(tty);
        let mut cmd = remote_connect_pty_command(&env, &log);
        cmd.env("RIMZ_REMOTE_GRACE_MS", if panel { "0" } else { "60000" });
        cmd.env("RIMZ_REMOTE_MIN_DISPLAY_MS", "100");
        cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
        cmd.env(
            "RIMZ_TEST_SSH_STDERR",
            "Shared connection to dev-box closed.\n",
        );
        cmd.env(
            "RIMZ_TEST_SSH_STDOUT",
            format!(
                "{}\x1b[?1049hROOM\x1b[?1049l[detached]",
                rimz::remote::tty::ROOM_OPEN_SENTINEL
            ),
        );
        let (output, _) = run_pty_command(pair, cmd);
        let mut parser = vt100::Parser::new(24.try_into().unwrap(), 100.try_into().unwrap(), 100);
        parser.process(output.as_bytes());
        let main = parser.screen().contents();
        assert!(!parser.screen().alternate_screen());
        assert!(
            main.lines().any(|line| line == "$ rimz remote connect dev"),
            "command row intact: {main:?}"
        );
        let lines = main.lines().collect::<Vec<_>>();
        let detach = lines
            .iter()
            .position(|line| *line == "[detached]")
            .expect("detach row");
        assert_eq!(lines[detach + 1], "rimz: detached from dev-box", "{main:?}");
    }
}

#[test]
fn supervised_reconnect_labels_each_surfaced_attach_but_never_piped_output() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    std::fs::write(&plan, "255\n0\n").unwrap();
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_PLAN", &plan);
    cmd.env("RIMZ_TEST_SSH_STDOUT", "host notice\r\n");
    cmd.env("RIMZ_REMOTE_GATETIME_MS", "0");
    let (output, _) = run_pty_command(remote_connect_pty(), cmd);
    let separators = output
        .lines()
        .filter(|line| line.contains("── dev-box:query-engine · "))
        .collect::<Vec<_>>();
    assert_eq!(separators.len(), 2, "{output:?}");
    assert!(!separators[0].contains("reconnected"));
    assert!(separators[1].contains("reconnected"));
    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_STDOUT", "host notice\n")
        .bounded_output()
        .unwrap();
    assert_eq!(out.stdout, b"host notice\n");
}

#[test]
fn supervised_unpainted_mux_error_is_replayed_as_text_on_the_main_screen() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_REMOTE_GRACE_MS", "0");
    cmd.env("RIMZ_REMOTE_MIN_DISPLAY_MS", "100");
    cmd.env(
        "RIMZ_TEST_SSH_STDOUT",
        format!(
            "{}\x1b[31mMUX FAILED\x1b]52;c;secret\x07",
            rimz::remote::tty::ROOM_OPEN_SENTINEL
        ),
    );
    let (output, _) = run_pty_command(remote_connect_pty(), cmd);
    let leave = output.find("\x1b[?1049l").unwrap();
    assert!(
        output[leave..].contains("MUX FAILED"),
        "unpainted error survives: {output:?}"
    );
    let reset = output.rfind(rimz::remote::tty::EMULATOR_RESET).unwrap()
        + rimz::remote::tty::EMULATOR_RESET.len();
    let separator = output[reset..].find("── ").unwrap();
    assert!(
        !output[reset..reset + separator].contains('\n'),
        "the replay's separator takes the panel's row, an unfinished tail adds no blank row: {output:?}"
    );
    assert!(
        !output[leave..].contains("secret"),
        "escapes are not replayed: {output:?}"
    );
}

#[test]
fn supervised_drain_does_not_wait_for_a_descendant_or_leak_its_late_bytes() {
    const SSH_TRACE_LATE_OUTPUT_HOLD: Duration = Duration::from_secs(3);

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    std::fs::write(&plan, "255\n0\n").unwrap();
    let mut child = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_LATE_OUTPUT", "1")
        .env("RIMZ_TEST_SSH_STDOUT", "current attach\n")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_main_invocations(&mut child, &log, 1, COMMAND_TIMEOUT);
    let first_attach_seen = Instant::now();
    let out = child.wait_with_output().unwrap();
    let drained_after = first_attach_seen.elapsed();
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("LATE-OLD-ATTACH"),
        "an ended attach must stop forwarding"
    );
    assert!(
        drained_after < SSH_TRACE_LATE_OUTPUT_HOLD,
        "inherited writers cannot hold the drain: {drained_after:?}"
    );
    assert!(out.status.success());
    assert_eq!(out.stdout, b"current attach\ncurrent attach\n");
    assert_eq!(main_invocation_count(&log), 2);
}

#[cfg(target_os = "linux")]
#[test]
fn supervised_idle_output_relay_sleeps_until_output_or_stop() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let mut child = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_SLEEP_MS", "2000")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let task = loop {
        let tasks = std::fs::read_dir(format!("/proc/{}/task", child.id())).unwrap();
        if let Some(task) = tasks.filter_map(Result::ok).find(|task| {
            std::fs::read_to_string(task.path().join("comm"))
                .is_ok_and(|name| name.trim() == "remote-output")
        }) {
            break task.path();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("output relay did not start");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let switches = || {
        std::fs::read_to_string(task.join("status"))
            .unwrap()
            .lines()
            .find_map(|line| line.strip_prefix("voluntary_ctxt_switches:"))
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap()
    };
    let before = switches();
    std::thread::sleep(Duration::from_millis(200));
    let wakeups = switches().saturating_sub(before);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(wakeups <= 3, "idle output relay woke {wakeups} times");
}

#[test]
fn supervised_output_relay_failure_kills_ssh_and_never_reconnects() {
    // The bound is the only proof of the kill, so ssh parks far past it: a lost kill fails by
    // seconds, not by poll lag.
    const KILL_BOUND: Duration = Duration::from_secs(3);
    const PARKED: Duration = Duration::from_secs(10);

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let mut child = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_STDOUT", "host text\n")
        .env("RIMZ_TEST_SSH_SLEEP_MS", PARKED.as_millis().to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    wait_for_main_invocations(&mut child, &log, 1, COMMAND_TIMEOUT);
    let first_attach_seen = Instant::now();
    let deadline = first_attach_seen + COMMAND_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("local relay failure must stop ssh");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let killed_after = first_attach_seen.elapsed();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.contains("local output relay"), "{stderr}");
    assert_eq!(status.code(), Some(1));
    assert!(
        killed_after < KILL_BOUND,
        "ssh was killed promptly: {killed_after:?}"
    );
    assert_eq!(
        main_invocation_count(&log),
        1,
        "no reconnect after local failure"
    );
}

#[test]
fn supervised_initial_transport_failure_retries_before_main_attach() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let master_plan = env.project_root.join("ssh-master.plan");
    std::fs::write(&master_plan, "255\n0\n").expect("write master plan");

    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_MASTER_PLAN", &master_plan)
        .env(
            "RIMZ_TEST_SSH_MASTER_STDERR",
            "ssh: connect to host dev-box port 22: Connection refused\n",
        )
        .bounded_output()
        .expect("run supervised remote connect");

    assert!(
        out.status.success(),
        "transport retry reaches the main attach\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log);
    let main_index = invocations
        .iter()
        .position(|argv| is_main_invocation(argv))
        .expect("main attach");
    assert_eq!(
        invocations[..main_index]
            .iter()
            .filter(|argv| is_master_invocation(argv))
            .count(),
        2,
        "both masters precede the main attach: {invocations:?}"
    );
}

#[test]
fn supervised_master_deadline_kills_and_repaces_a_hung_attempt() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let ready = env.project_root.join("control-master-ready");
    let master_plan = env.project_root.join("ssh-master.plan");
    let ready_plan = env.project_root.join("ssh-master-ready.plan");
    std::fs::write(&master_plan, "0\n255\n").expect("write master plan");
    std::fs::write(&ready_plan, "0\n").expect("write ready plan");

    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_MASTER_PLAN", &master_plan)
        .env("RIMZ_TEST_SSH_MASTER_READY_PLAN", &ready_plan)
        .env(
            "RIMZ_TEST_SSH_MASTER_STDERR",
            "Permission denied (publickey).\n",
        )
        .env("RIMZ_TEST_CONTROL_MASTER_READY", &ready)
        .env("RIMZ_REMOTE_MASTER_TIMEOUT_MS", "50")
        .bounded_output()
        .expect("run supervised remote connect");

    assert!(
        out.status.success(),
        "the replacement attempt reaches interactive fallback\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(master_invocation_count(&log), 2);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("SSH connect timed out after 0s"),
        "the attempt timeout is visible"
    );
}

#[test]
fn tun_route_skips_dead_tcp_checkpoint_and_keeps_ssh_retries_fast() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let master_plan = env.project_root.join("ssh-master.plan");
    let (ssh_config, _endpoint) = reserved_ssh_endpoint(&env);
    std::fs::write(&master_plan, "255\n0\n").expect("write master plan");

    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_G_FILE", &ssh_config)
        .env("RIMZ_TEST_SSH_MASTER_PLAN", &master_plan)
        .env(
            "RIMZ_TEST_SSH_MASTER_STDERR",
            "ssh: connect to host dev-box port 22: Connection refused\n",
        )
        .env("RIMZ_REMOTE_DIAL_MS", "10")
        .env("RIMZ_REMOTE_REACHABLE_RETRY_MS", "20")
        .env("RIMZ_REMOTE_TUN", "utun3")
        .bounded_output()
        .expect("run TUN-routed remote connect");

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success());
    assert!(
        !stderr.contains("waiting for network"),
        "TUN bypass keeps the network up, so every retry takes reachable pacing\nstderr:\n{stderr}"
    );
    assert_eq!(master_invocation_count(&log), 2);
    assert!(
        stderr.contains("server route to dev-box uses TUN utun3 — TCP check skipped"),
        "the skipped checkpoint is visible"
    );
}

#[test]
fn supervised_connect_restores_tty_and_resets_emulator_after_retry() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let tty_state_log = env.project_root.join("ssh-tty-state.log");
    std::fs::write(&plan, "255\n0\n").expect("write ssh plan");
    let pair = remote_connect_pty();
    put_pty_in_raw_mode(&pair);
    let damaged = read_pty_termios(&pair);
    assert!(rimz::remote::tty::termios_damaged(
        damaged.input_flags,
        damaged.output_flags,
        damaged.local_flags
    ));

    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_PLAN", &plan);
    cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
    cmd.env("RIMZ_TEST_SSH_TTY_STATE_LOG", &tty_state_log);
    cmd.env("RIMZ_REMOTE_GATETIME_MS", "0");
    let (output, settings) = run_pty_command(pair, cmd);

    assert_eq!(main_invocation_count(&log), 2, "one retry: {output}");
    assert_eq!(
        std::fs::read_to_string(&tty_state_log)
            .expect("read ssh tty state log")
            .lines()
            .collect::<Vec<_>>(),
        ["sane", "sane"],
        "the guard repairs entry and restores before retry"
    );
    assert_eq!(
        output.matches(rimz::remote::tty::EMULATOR_RESET).count(),
        2,
        "every completed session resets the emulator: {output:?}"
    );
    assert_shell_tty(&settings);
}

#[test]
fn supervised_clean_exit_resets_dirty_emulator() {
    const DIRTY_EMULATOR: &str = "\x1b[?1006h\x1b[?1003h";

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let pair = remote_connect_pty();
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
    cmd.env("RIMZ_TEST_SSH_STDOUT", DIRTY_EMULATOR);

    let (output, settings) = run_pty_command(pair, cmd);

    assert_eq!(main_invocation_count(&log), 1, "one attach: {output}");
    let enabled_at = output
        .find(DIRTY_EMULATOR)
        .expect("foreground client enables mouse reporting");
    let reset_at = output
        .rfind(rimz::remote::tty::EMULATOR_RESET)
        .expect("supervisor resets the emulator");
    assert!(
        enabled_at < reset_at,
        "the reset follows the dirty child output: {output:?}"
    );
    assert_eq!(
        output.matches(rimz::remote::tty::EMULATOR_RESET).count(),
        1,
        "the cleanly detached session resets the emulator: {output:?}"
    );
    assert_shell_tty(&settings);
}

#[test]
fn supervised_clean_exit_preserves_shell_typeahead() {
    const TYPEAHEAD: &[u8] = b"rimz-shell-typeahead\n";

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let pair = remote_connect_pty();
    let tty_name = pair.master.tty_name().expect("remote connect pty name");
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
    cmd.env("RIMZ_TEST_SSH_SLEEP_MS", "500");

    let mut child = pair.slave.spawn_command(cmd).expect("spawn remote connect");
    drop(pair.slave);
    let mut writer = pair.master.take_writer().expect("take pty writer");
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    while main_invocation_count(&log) < 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(main_invocation_count(&log), 1, "attach did not start");
    writer.write_all(TYPEAHEAD).expect("queue shell typeahead");
    writer.flush().expect("flush shell typeahead");
    drop(writer);

    let status = loop {
        if let Some(status) = child.try_wait().expect("poll remote connect") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    .expect("remote connect timed out");
    assert!(status.success(), "remote connect failed with {status:?}");

    let mut tty = std::fs::File::open(tty_name).expect("reopen remote connect pty");
    nix::fcntl::fcntl(
        &tty,
        nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
    )
    .expect("set remote connect pty nonblocking");
    let mut queued = vec![0u8; TYPEAHEAD.len()];
    tty.read_exact(&mut queued)
        .expect("read queued shell typeahead");
    assert_eq!(queued, TYPEAHEAD);
}

#[test]
fn supervised_reconnect_discards_stale_terminal_replies_and_preserves_user_input() {
    const TERMINAL_REPLIES: &[u8] = b"\x1b[?62;22;52c\x1b[>1;10;0c\x1bP>|ghostty 1.3.1\x1b\\";
    const USER_MARKER: &[u8] = b"RIMZ-USER-MARKER\r";

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let exit_plan = env.project_root.join("ssh-trace.plan");
    let stdin_plan = env.project_root.join("ssh-stdin.plan");
    let stdin_log = env.project_root.join("ssh-stdin.log");
    std::fs::write(&exit_plan, "255\n0\n").expect("write ssh plan");
    std::fs::write(&stdin_plan, "ignore\nrecord\n").expect("write ssh stdin plan");

    let pair = remote_connect_pty();
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_PLAN", &exit_plan);
    cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
    cmd.env("RIMZ_TEST_SSH_SLEEP_MS", "1000");
    cmd.env("RIMZ_TEST_SSH_STDIN_PLAN", &stdin_plan);
    cmd.env("RIMZ_TEST_SSH_STDIN_LOG", &stdin_log);
    cmd.env("RIMZ_TEST_SSH_STDIN_UNTIL", "RIMZ-USER-MARKER");
    cmd.env("RIMZ_REMOTE_GATETIME_MS", "0");

    let mut child = pair.slave.spawn_command(cmd).expect("spawn remote connect");
    drop(pair.slave);
    let terminal = FakeTerminal::new(pair.master, Duration::ZERO, Answers::StatusOnly);
    let deadline = Instant::now() + COMMAND_TIMEOUT;

    while main_invocation_count(&log) < 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(main_invocation_count(&log), 1, "first attach did not start");
    terminal.write(TERMINAL_REPLIES);

    while main_invocation_count(&log) < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        main_invocation_count(&log),
        2,
        "replacement attach did not start"
    );
    terminal.write(USER_MARKER);

    let status = loop {
        if let Some(status) = child.try_wait().expect("poll remote connect") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = String::from_utf8_lossy(&terminal.finish()).into_owned();
    let status = status.unwrap_or_else(|| panic!("remote connect timed out; output:\n{output}"));
    assert!(
        status.success(),
        "remote connect failed with {status:?}; output:\n{output}"
    );
    assert_eq!(main_invocation_count(&log), 2, "one retry: {output}");

    let recorded = std::fs::read(&stdin_log).expect("read replacement attach stdin");
    assert!(
        recorded
            .windows(b"RIMZ-USER-MARKER".len())
            .any(|window| window == b"RIMZ-USER-MARKER"),
        "post-handoff user input was not delivered: {recorded:?}"
    );
    for stale_token in [
        b"62;22;52".as_slice(),
        b"1;10;0c".as_slice(),
        b"ghostty".as_slice(),
    ] {
        assert!(
            !recorded
                .windows(stale_token.len())
                .any(|window| window == stale_token),
            "stale terminal reply reached replacement attach: {recorded:?}"
        );
    }
}

#[test]
fn supervised_reconnect_fences_replies_that_arrive_after_the_quiet_window() {
    const TERMINAL_REPLIES: &[u8] = b"\x1b[?62;22;52c\x1b[>1;10;0c\x1bP>|ghostty 1.3.1\x1b\\";
    const USER_MARKER: &[u8] = b"RIMZ-USER-MARKER\r";

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let exit_plan = env.project_root.join("ssh-trace.plan");
    let stdin_plan = env.project_root.join("ssh-stdin.plan");
    let stdin_log = env.project_root.join("ssh-stdin.log");
    std::fs::write(&exit_plan, "255\n0\n").expect("write ssh plan");
    std::fs::write(&stdin_plan, "ignore\nrecord\n").expect("write ssh stdin plan");

    let pair = remote_connect_pty();
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_PLAN", &exit_plan);
    cmd.env("RIMZ_TEST_SSH_RAW_TTY", "1");
    cmd.env("RIMZ_TEST_SSH_SLEEP_MS", "0");
    cmd.env("RIMZ_TEST_SSH_STDIN_PLAN", &stdin_plan);
    cmd.env("RIMZ_TEST_SSH_STDIN_LOG", &stdin_log);
    cmd.env("RIMZ_TEST_SSH_STDIN_UNTIL", "RIMZ-USER-MARKER");
    cmd.env("RIMZ_REMOTE_GATETIME_MS", "0");

    let mut child = pair.slave.spawn_command(cmd).expect("spawn remote connect");
    drop(pair.slave);
    let terminal = FakeTerminal::new(pair.master, Duration::from_millis(400), Answers::StatusOnly);
    let deadline = Instant::now() + COMMAND_TIMEOUT;

    while main_invocation_count(&log) < 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(main_invocation_count(&log), 1, "first attach did not start");

    terminal.wait_for_status_query(1, deadline);
    terminal.write_after(Duration::from_millis(300), TERMINAL_REPLIES);
    terminal.write_after_next_reply(USER_MARKER);

    while main_invocation_count(&log) < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        main_invocation_count(&log),
        2,
        "replacement attach did not start"
    );
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll remote connect") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = String::from_utf8_lossy(&terminal.finish()).into_owned();
    let status = status.unwrap_or_else(|| panic!("remote connect timed out; output:\n{output}"));
    assert!(
        status.success(),
        "remote connect failed with {status:?}; output:\n{output}"
    );
    assert_eq!(main_invocation_count(&log), 2, "one retry: {output}");

    let recorded = std::fs::read(&stdin_log).expect("read replacement attach stdin");
    assert!(
        recorded
            .windows(b"RIMZ-USER-MARKER".len())
            .any(|window| window == b"RIMZ-USER-MARKER"),
        "post-handoff user input was not delivered: {recorded:?}"
    );
    for stale_token in [
        b"62;22;52".as_slice(),
        b"1;10;0c".as_slice(),
        b"ghostty".as_slice(),
    ] {
        assert!(
            !recorded
                .windows(stale_token.len())
                .any(|window| window == stale_token),
            "stale terminal reply reached replacement attach: {recorded:?}"
        );
    }
}

#[test]
fn recovery_panel_checks_the_configured_http_204_endpoint() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    std::fs::write(&plan, "255\n0\n").expect("write plan");
    let (probe_url, stop, probe_thread) = http_204_probe();

    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env("RIMZ_TEST_SSH_PLAN", &plan);
    cmd.env("RIMZ_TEST_SSH_SLEEP_MS", "20");
    cmd.env("RIMZ_REMOTE_GATETIME_MS", "0");
    cmd.env("RIMZ_REMOTE_DIAL_MS", "25");
    cmd.env("RIMZ_REMOTE_REACHABLE_RETRY_MS", "500");
    cmd.env("RIMZ_REMOTE_GRACE_MS", "0");
    cmd.env("RIMZ_REMOTE_MIN_DISPLAY_MS", "500");
    cmd.env("RIMZ_REMOTE_INTERNET_PROBE", &probe_url);
    let (output, _) = run_pty_command(remote_connect_pty(), cmd);
    stop.store(true, Ordering::Relaxed);
    probe_thread.join().expect("join HTTP probe");

    assert!(
        output.contains("Internet"),
        "internet row is visible: {output}"
    );
    assert!(
        output.contains("127.0.0.1"),
        "the row labels the configured URL host: {output}"
    );
    assert!(
        output.contains("✓  Internet"),
        "HTTP 204 settles the checkpoint as reachable: {output}"
    );
    let mouse_on = output
        .find("\x1b[?1000h\x1b[?1006h")
        .expect("recovery panel enables click and wheel reporting");
    let mouse_off = output
        .rfind("\x1b[?1006l\x1b[?1000l")
        .expect("handoff cleanup disables click and wheel reporting");
    assert!(
        mouse_on < mouse_off,
        "mouse reporting remains owned until handoff cleanup: {output:?}"
    );
}

#[test]
fn probe_stream_waits_for_control_master_before_starting() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let ready = env.project_root.join("control-master-ready");
    let fallback = env.project_root.join("probe-before-master");
    let out = env
        .rimz()
        .args(["remote", "connect", "dev-box:query-engine", "--attach"])
        .env("RIMZ_SSH_BIN", ssh_shim())
        .env("RIMZ_TEST_SSH_LOG", &log)
        .env("RIMZ_TEST_CONTROL_MASTER_READY", &ready)
        .env("RIMZ_TEST_CONTROL_MASTER_READY_DELAY_MS", "100")
        .env("RIMZ_TEST_PROBE_BEFORE_MASTER", &fallback)
        .env("RIMZ_TEST_WAIT_FOR_PROBE_MS", "5000")
        .env("RIMZ_REMOTE_DIAL_MS", "0")
        .env("RIMZ_REMOTE_PROBE_MS", "20")
        .env("RIMZ_REMOTE_PROBE_TIMEOUT_MS", "20")
        .env("TERM", "xterm-256color")
        .bounded_output()
        .expect("run rimz remote connect --attach");
    assert!(
        out.status.success(),
        "shim exits 0\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !fallback.exists(),
        "probe stream must not start before ControlMaster is ready"
    );
    let invocations = shim_invocations(&log);
    let probe_index = invocations
        .iter()
        .position(|argv| is_probe_invocation(argv))
        .expect("probe stream invocation");
    let master_index = invocations
        .iter()
        .position(|argv| is_master_invocation(argv))
        .expect("master invocation");
    assert!(
        probe_index > master_index,
        "probe stream starts only after the master begins: {invocations:?}"
    );
    assert!(
        invocations[..probe_index]
            .iter()
            .any(|argv| is_control_check_invocation(argv)),
        "readiness checks run before the probe stream: {invocations:?}"
    );
}

#[test]
fn established_link_drop_reconnects_and_notifies_once() {
    let env = Env::new();
    write_link_notify_command_config(&env);
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let notify_log = env.project_root.join("notify.log");
    let port = available_forward_probe_port();
    // First session drops the link (255), the reattach detaches cleanly (0).
    std::fs::write(&plan, "255\n0\n").expect("write plan");
    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_SLEEP_MS", "500")
        .env("RIMZ_REMOTE_PROBE_MS", "10")
        .env("RIMZ_TEST_PROBE_PORT", port.to_string())
        .env("RIMZ_TEST_PROBE_PORTS_SCOPE", "room")
        .env("RIMZ_TEST_WAIT_FOR_PROBE_MS", "500")
        .env("RIMZ_REMOTE_GATETIME_MS", "20")
        .env("RIMZ_NOTIFY_TEST_LOG", &notify_log)
        .bounded_output()
        .expect("run rimz remote connect --attach");
    assert!(
        out.status.success(),
        "reconnect ends on the clean detach\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log)
        .into_iter()
        .filter(|argv| is_main_invocation(argv))
        .collect::<Vec<_>>();
    assert_eq!(invocations.len(), 2, "dropped once, reattached once");
    let all_invocations = shim_invocations(&log);
    assert_eq!(
        tunnel_forward_ports(&log),
        [port, port],
        "a replacement master reopens the same forward"
    );
    assert_eq!(
        all_invocations
            .iter()
            .filter(|argv| is_master_invocation(argv))
            .count(),
        2,
        "the initial and recovery masters prove both connections"
    );
    assert!(
        invocations
            .iter()
            .all(|invocation| snippet(invocation).contains("RIMZ_REMOTE_SUPERVISED=1")),
        "every supervised attach marks its remote wrapper: {invocations:?}"
    );
    assert!(
        !snippet(&invocations[0]).contains("RIMZ_REMOTE_RECONNECT"),
        "the initial attach stays attended: {:?}",
        invocations[0]
    );
    assert!(
        snippet(&invocations[1]).contains("export RIMZ_REMOTE_RECONNECT=1;"),
        "the retry is marked unattended: {:?}",
        invocations[1]
    );
    assert!(
        invocations[1]
            .iter()
            .any(|arg| arg.starts_with("ControlPath=")),
        "the tty reattach reuses the confirmed master: {:?}",
        invocations[1]
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("reconnecting"),
        "the supervisor narrates the retry: {stderr}"
    );
    assert!(
        !stderr.contains("network to dev-box restored — reconnecting now"),
        "a plain link drop must not report network restoration: {stderr}"
    );
    assert!(
        stderr.contains("reattached to dev-box"),
        "plain mode reports the successful handoff: {stderr}"
    );

    let text = wait_for_notify_log(
        &notify_log,
        &[
            "link_lost|RimZ: remote link lost|SSH to dev-box dropped; reconnecting.",
            "link_restored|RimZ: remote link restored|SSH to dev-box is responsive again.",
        ],
    );
    assert_eq!(text.matches("link_lost|").count(), 1, "lost edge: {text}");
    assert_eq!(
        text.matches("link_restored|").count(),
        1,
        "restored edge: {text}"
    );
    assert!(
        text.find("link_lost|") < text.find("link_restored|"),
        "restore follows loss: {text}"
    );
}

#[test]
fn repeated_transport_drops_narrate_each_retry_but_notify_once() {
    let env = Env::new();
    write_link_notify_command_config(&env);
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let notify_log = env.project_root.join("notify.log");
    std::fs::write(&plan, "255\n255\n0\n").expect("write plan");
    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_STDERR", "Connection reset by peer")
        .env("RIMZ_REMOTE_GATETIME_MS", "60000")
        .env("RIMZ_NOTIFY_TEST_LOG", &notify_log)
        .bounded_output()
        .expect("run repeated transport drops");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(main_invocation_count(&log), 3);
    assert_eq!(master_invocation_count(&log), 3);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let narration = "rimz: link to dev-box lost — Connection reset by peer — reconnecting in the background; Ctrl-C stops";
    assert_eq!(stderr.matches(narration).count(), 2, "{stderr}");
    let text = wait_for_notify_log(
        &notify_log,
        &["link_lost|RimZ: remote link lost|SSH to dev-box dropped; reconnecting."],
    );
    assert_eq!(text.matches("link_lost|").count(), 1, "{text}");
    assert!(
        !text.contains("link_restored|"),
        "no responsive session between drops: {text}"
    );
}

#[test]
fn control_socket_preparation_failure_is_narrated_before_retry() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let stderr_path = env.project_root.join("remote.stderr");
    let blocker = env.runtime_root.join("rimz/link");
    std::fs::create_dir_all(blocker.parent().expect("runtime parent")).expect("create runtime");
    std::fs::write(&blocker, "not a directory").expect("block control directory");
    let mut child = remote_connect_command(&env, &log)
        .stderr(std::fs::File::create(&stderr_path).expect("capture stderr"))
        .spawn()
        .expect("spawn remote connect");
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    loop {
        let stderr = std::fs::read_to_string(&stderr_path).expect("read stderr");
        if stderr.contains("rimz: connect to dev-box failed — preparing SSH control socket:") {
            break;
        }
        if Instant::now() >= deadline || child.try_wait().expect("poll remote connect").is_some() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("missing preparation failure: {stderr}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(master_invocation_count(&log), 0);
    std::fs::remove_file(&blocker).expect("unblock control directory");
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().expect("poll retry") {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("control socket retry did not finish");
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.success());
    assert_eq!(master_invocation_count(&log), 1);
}

#[test]
fn supervised_ssh_escape_ends_connect_without_reconnecting() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    std::fs::write(&plan, "escape\n").expect("write plan");
    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_REMOTE_GATETIME_MS", "60000")
        .bounded_output()
        .expect("run rimz remote connect --attach");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "escape exits cleanly: {stderr}");
    assert_eq!(
        main_invocation_count(&log),
        1,
        "the escape must not start another attach: {stderr}"
    );
    assert_eq!(
        master_invocation_count(&log),
        1,
        "the escape must not start another master: {stderr}"
    );
    assert!(stderr.contains("rimz: detached from dev-box"), "{stderr}");
    assert!(!stderr.contains("link to dev-box lost"), "{stderr}");
    assert!(!stderr.contains("reconnecting"), "{stderr}");
    assert!(
        !stderr.contains("SSH control connection dropped"),
        "{stderr}"
    );
    let invocations = shim_invocations(&log);
    let control_path = invocations
        .iter()
        .find(|argv| is_master_invocation(argv))
        .expect("master invocation")
        .iter()
        .find_map(|arg| arg.strip_prefix("ControlPath="))
        .expect("master control path");
    assert!(
        !Path::new(control_path).exists(),
        "the escape removes the master's control socket: {control_path}"
    );
}

#[test]
fn established_mux_disconnect_reconnects() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let port = available_forward_probe_port();
    // A mux client can return nonzero when its server process dies; the
    // replacement attach then detaches cleanly.
    std::fs::write(&plan, "1\n0\n").expect("write plan");
    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_SLEEP_MS", "500")
        .env("RIMZ_REMOTE_PROBE_MS", "10")
        .env("RIMZ_TEST_PROBE_PORT", port.to_string())
        .env("RIMZ_TEST_PROBE_PORTS_SCOPE", "room")
        .env("RIMZ_TEST_WAIT_FOR_PROBE_MS", "500")
        .env("RIMZ_REMOTE_GATETIME_MS", "20")
        .bounded_output()
        .expect("run rimz remote connect --attach");

    assert!(
        out.status.success(),
        "established mux disconnect reattaches\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        main_invocation_count(&log),
        2,
        "the established mux client is reattached once"
    );
    assert_eq!(
        master_invocation_count(&log),
        1,
        "reattach reuses the healthy SSH connection"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("remote session on dev-box disconnected"),
        "the mux disconnect is reported separately from link loss"
    );
    assert!(!stderr.contains("link to dev-box lost"), "{stderr}");
    assert_eq!(
        tunnel_forward_ports(&log),
        [port],
        "reattach leaves the held forward alone"
    );
}

#[test]
fn remote_session_loss_sentinel_exits_cleanly_without_a_tty() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    std::fs::write(
        &plan,
        format!("{}\n", rimz::remote::REMOTE_SESSION_LOST_EXIT),
    )
    .expect("write plan");
    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_SLEEP_MS", "80")
        .env("RIMZ_REMOTE_GATETIME_MS", "20")
        .bounded_output()
        .expect("run rimz remote connect --attach");

    assert!(
        out.status.success(),
        "lost remote session exits cleanly\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        main_invocation_count(&log),
        1,
        "the room-end sentinel should not start another attach"
    );
    assert_eq!(
        master_invocation_count(&log),
        1,
        "the initial attach uses one healthy SSH connection"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rimz: remote room on dev-box ended"),
        "{stderr}"
    );
    assert!(!stderr.contains("[Y/n]"), "{stderr}");
}

#[test]
fn supervised_remote_attach_ends_when_the_session_disappears() {
    for attach_exits in [false, true] {
        let env = Env::new();
        let session = "watchdog-room";
        let zellij_log = env.project_root.join("zellij-watchdog.log");
        let watchdog_state = env.project_root.join("zellij-watchdog.state");
        let mut command = env.rimz();
        command
            .args(["--mux", "zellij", "attach", "--attach", session])
            .env(
                "RIMZ_ZELLIJ_BIN",
                crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
            )
            .env("RIMZ_TEST_ZELLIJ_LOG", &zellij_log)
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
                format!("{session} [Created 1s ago]\n"),
            )
            .env(
                "RIMZ_TEST_ZELLIJ_LIST_PANES",
                r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
            )
            .env("RIMZ_TEST_ZELLIJ_WATCHDOG_STATE", &watchdog_state)
            .env("RIMZ_REMOTE_SUPERVISED", "1")
            // A client that exits on its own must beat the next check, or the pass ends
            // through the watchdog's kill and never reaches the post-exit check it exists for.
            .env(
                "RIMZ_REMOTE_WATCHDOG_MS",
                if attach_exits { "1000" } else { "25" },
            );
        if attach_exits {
            command.env("RIMZ_TEST_ZELLIJ_WATCHDOG_ATTACH_EXIT", "1");
        }
        let output = command
            .bounded_output()
            .expect("run supervised remote attach wrapper");

        assert_eq!(
            output.status.code(),
            Some(rimz::remote::REMOTE_SESSION_LOST_EXIT),
            "watchdog should translate room end to the remote sentinel\nstderr:\n{}",
            String::from_utf8_lossy(&output.stderr),
        );
        let trace = std::fs::read_to_string(&zellij_log).expect("read zellij watchdog trace");
        assert!(
            trace.contains("\tattach\twatchdog-room\toptions"),
            "{trace}"
        );
        assert!(
            trace.matches("\tlist-sessions").count() >= 2,
            "watchdog should observe the live session and its disappearance: {trace}"
        );
        let pid = std::fs::read_to_string(watchdog_state.with_extension("pid"))
            .expect("read attached client pid")
            .parse::<i32>()
            .expect("parse attached client pid");
        assert_eq!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
            Err(nix::errno::Errno::ESRCH),
            "mux client should be dead before the wrapper returns",
        );
    }
}

#[test]
fn watchdog_sentinel_reaches_the_local_clean_exit_policy() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let zellij_log = env.project_root.join("zellij-watchdog.log");
    let watchdog_state = env.project_root.join("zellij-watchdog.state");
    let session = "watchdog-room";
    let mut cmd = remote_connect_pty_command(&env, &log);
    cmd.env(
        "RIMZ_TEST_SSH_EXEC_ARGV",
        format!(
            "{}\t--mux\tzellij\tattach\t--attach\t{session}",
            env.rimz_bin().display()
        ),
    );
    cmd.env(
        "RIMZ_ZELLIJ_BIN",
        crate::common::cargo_bin("zellij-trace", env!("CARGO_BIN_EXE_zellij-trace")),
    );
    cmd.env("RIMZ_TEST_ZELLIJ_LOG", &zellij_log);
    cmd.env(
        "RIMZ_TEST_ZELLIJ_LIST_SESSIONS",
        format!("{session} [Created 1s ago]\n"),
    );
    cmd.env(
        "RIMZ_TEST_ZELLIJ_LIST_PANES",
        r#"[{"id":1,"is_plugin":false,"tab_id":1,"title":"sh"}]"#,
    );
    cmd.env("RIMZ_TEST_ZELLIJ_WATCHDOG_STATE", &watchdog_state);
    cmd.env("RIMZ_REMOTE_SUPERVISED", "1");
    cmd.env("RIMZ_REMOTE_WATCHDOG_MS", "25");
    cmd.env("RIMZ_REMOTE_GATETIME_MS", "20");

    let (output, settings) = run_pty_command(remote_connect_pty(), cmd);

    assert_eq!(main_invocation_count(&log), 1, "{output}");
    assert_eq!(master_invocation_count(&log), 1, "{output}");
    assert!(
        output.contains("rimz: remote room on dev-box ended"),
        "{output}"
    );
    assert!(!output.contains("[Y/n]"), "{output}");
    assert!(
        !output.contains("recreating the remote session"),
        "{output}"
    );
    let trace = std::fs::read_to_string(&zellij_log).expect("read zellij watchdog trace");
    assert!(
        trace.contains("\tattach\twatchdog-room\toptions"),
        "{trace}"
    );
    let pid = std::fs::read_to_string(watchdog_state.with_extension("pid"))
        .expect("read attached client pid")
        .parse::<i32>()
        .expect("parse attached client pid");
    assert_eq!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH),
        "mux client should be dead before the local supervisor returns",
    );
    assert_shell_tty(&settings);
}

#[test]
fn ctrl_c_during_remote_setup_prompt_stops_without_reconnecting() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    std::fs::write(&plan, "255\n0\n").expect("write plan");

    let out = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_STDERR", "Killed by signal 2.\n")
        .bounded_output()
        .expect("run interrupted remote connect");

    assert!(
        out.status.success(),
        "Ctrl-C exits the supervisor cleanly\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        main_invocation_count(&log),
        1,
        "the attended attach is not retried"
    );
    assert_eq!(
        master_invocation_count(&log),
        1,
        "the supervisor does not open a recovery connection"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("reconnecting"),
        "an explicit interruption is not presented as link loss"
    );
}

#[test]
fn unreachable_endpoint_holds_until_restored_then_reconnects_immediately() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let master_plan = env.project_root.join("ssh-master.plan");
    let stderr_path = env.project_root.join("stderr.log");
    let (ssh_config, endpoint) = reserved_ssh_endpoint(&env);
    std::fs::write(&plan, "255\n0\n").expect("write plan");
    std::fs::write(&master_plan, "0\n").expect("script initial master before outage");
    let stderr = std::fs::File::create(&stderr_path).expect("create stderr log");

    let mut child = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_MASTER_PLAN", &master_plan)
        .env("RIMZ_TEST_SSH_MASTER_DIAL", "1")
        .env("RIMZ_TEST_SSH_G_FILE", &ssh_config)
        .env("RIMZ_REMOTE_GATETIME_MS", "0")
        .env("RIMZ_REMOTE_DIAL_MS", "25")
        .stdout(Stdio::piped())
        .stderr(stderr)
        .spawn()
        .expect("spawn supervised remote connect");

    wait_for_main_invocations(&mut child, &log, 1, COMMAND_TIMEOUT);
    wait_for_notify_log(
        &stderr_path,
        &["network to dev-box lost — waiting for network"],
    );
    assert_eq!(
        main_invocation_count(&log),
        1,
        "an unreachable endpoint must not consume reconnect attempts"
    );

    socket::listen(&endpoint, Backlog::MAXCONN).expect("restore reachable endpoint");
    wait_for_main_invocations(&mut child, &log, 2, Duration::from_secs(3));
    let out = child.wait_with_output().expect("wait for clean reattach");
    let stderr = std::fs::read_to_string(&stderr_path).expect("read stderr log");

    assert!(
        out.status.success(),
        "restored endpoint reattaches cleanly\nstderr:\n{}",
        stderr
    );
    assert!(
        stderr.contains("network to dev-box lost — waiting for network; Ctrl-C stops"),
        "the unreachable hold is visible: {stderr}"
    );
    assert!(
        stderr.contains("network to dev-box restored — reconnecting now"),
        "the network edge is visible: {stderr}"
    );
}

#[test]
fn unreachable_endpoint_retries_at_the_hold_cap() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let (ssh_config, _endpoint) = reserved_ssh_endpoint(&env);
    std::fs::write(&plan, "255\n0\n").expect("write plan");

    let mut child = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_G_FILE", &ssh_config)
        .env("RIMZ_REMOTE_GATETIME_MS", "0")
        .env("RIMZ_REMOTE_BACKOFF_CAP_MS", "200")
        .env("RIMZ_REMOTE_DIAL_MS", "25")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn supervised remote connect");

    wait_for_main_invocations(&mut child, &log, 2, COMMAND_TIMEOUT);
    let out = child.wait_with_output().expect("wait for capped retry");

    assert!(
        out.status.success(),
        "hold-cap retry exits cleanly\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        main_invocation_count(&log),
        2,
        "the safety valve launches one retry while dials stay unreachable"
    );
}

#[test]
fn reachable_endpoint_retries_failed_masters_without_stderr_spam() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("ssh-trace.plan");
    let master_plan = env.project_root.join("ssh-master.plan");
    let (ssh_config, endpoint) = reserved_ssh_endpoint(&env);
    socket::listen(&endpoint, Backlog::MAXCONN).expect("answer endpoint dials");
    std::fs::write(&plan, "255\n0\n").expect("write plan");
    std::fs::write(&master_plan, "255\n255\n0\n").expect("write master plan");

    let mut child = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_PLAN", &plan)
        .env("RIMZ_TEST_SSH_MASTER_PLAN", &master_plan)
        .env(
            "RIMZ_TEST_SSH_MASTER_STDERR",
            "ssh: connect to host dev-box port 22: Connection refused\n",
        )
        .env("RIMZ_TEST_SSH_G_FILE", &ssh_config)
        .env("RIMZ_REMOTE_GATETIME_MS", "0")
        .env("RIMZ_REMOTE_BACKOFF_CAP_MS", "500")
        .env("RIMZ_REMOTE_REACHABLE_RETRY_MS", "1")
        .env("RIMZ_REMOTE_DIAL_MS", "20")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn supervised remote connect");

    wait_for_main_invocations(&mut child, &log, 2, COMMAND_TIMEOUT);
    let out = child.wait_with_output().expect("wait for clean reattach");
    assert!(
        out.status.success(),
        "background-master reconnect ends cleanly\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let invocations = shim_invocations(&log);
    assert_eq!(
        invocations
            .iter()
            .filter(|argv| is_master_invocation(argv))
            .count(),
        4,
        "two initial failures, one initial success, and one recovery master: {invocations:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(
        stderr.matches("Connection refused").count(),
        1,
        "identical fast failures report only on transition: {stderr}"
    );
}

#[test]
fn reachable_host_and_probe_blackout_kill_a_zombie_transport() {
    // The bound is the only proof that the kill came in time, so ssh parks far past it: a lost
    // or late kill fails at the bound, never at the park.
    const KILL_BOUND: Duration = Duration::from_secs(5);
    const PARKED: Duration = Duration::from_secs(15);

    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let stderr_path = env.project_root.join("stderr.log");
    let ready = env.project_root.join("control-master-ready");
    let ssh_config = env.project_root.join("ssh-config.txt");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind reachable endpoint");
    let address = listener.local_addr().expect("dial address");
    std::fs::write(
        &ssh_config,
        format!("hostname 127.0.0.1\nport {}\n", address.port()),
    )
    .expect("write ssh config fixture");
    let stderr = std::fs::File::create(&stderr_path).expect("create stderr log");

    let mut child = remote_connect_command(&env, &log)
        .env("RIMZ_TEST_SSH_G_FILE", &ssh_config)
        .env("RIMZ_TEST_CONTROL_MASTER_READY", &ready)
        .env("RIMZ_TEST_PROBE_SILENT_AFTER_ACKS", "2")
        .env("RIMZ_TEST_SSH_SLEEP_MS", PARKED.as_millis().to_string())
        .env("RIMZ_REMOTE_PROBE_MS", "20")
        .env("RIMZ_REMOTE_PROBE_TIMEOUT_MS", "20")
        .env("RIMZ_REMOTE_BLACKOUT_MS", "100")
        .env("RIMZ_REMOTE_GATETIME_MS", "0")
        .env("RIMZ_REMOTE_DIAL_MS", "50")
        .stdout(Stdio::null())
        .stderr(stderr)
        .spawn()
        .expect("spawn supervised remote connect");

    wait_for_main_invocations(&mut child, &log, 1, COMMAND_TIMEOUT);
    let first_attach_seen = Instant::now();
    wait_for_main_invocations(&mut child, &log, 2, KILL_BOUND);
    let reattached_after = first_attach_seen.elapsed();
    child
        .kill()
        .expect("stop supervisor after observing reattach");
    child.wait().expect("reap supervisor");
    let stderr = std::fs::read_to_string(&stderr_path).expect("read stderr log");
    assert!(
        stderr.contains(
            "link to dev-box confirmed dead — host reachable, session silent; reconnecting now"
        ),
        "zombie kill notice after reattach in {reattached_after:?}: {stderr}"
    );
}

#[test]
fn remote_web_missing_binary_points_at_setup_after_supervised_master() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let out = remote_web_command(&env, &log)
        .env("RIMZ_TEST_SSH_WEB_PREP_STATUS", "127")
        .bounded_output()
        .expect("run remote web prep without rimz");

    assert_eq!(out.status.code(), Some(127));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rimz is not installed on dev-box"),
        "{stderr}"
    );
    assert!(
        stderr.contains("rimz remote setup dev-box:query-engine"),
        "{stderr}"
    );
    assert_eq!(master_invocation_count(&log), 1, "one supervised master");
    assert_eq!(web_prep_invocation_count(&log), 1, "one preparation");
    assert_eq!(tunnel_invocation_count(&log), 0, "no tunnel opens");
}

#[test]
fn remote_web_explicit_port_in_use_fails_before_any_tunnel() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("hold explicit web port");
    let port = listener.local_addr().expect("held address").port();
    let out = remote_web_command(&env, &log)
        .args(["--web-port", &port.to_string()])
        .bounded_output()
        .expect("run remote web with an occupied port");

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("binding local web tunnel relay"),
        "{stderr}"
    );
    assert!(out.stdout.is_empty(), "no URL on relay bind failure");
    assert_eq!(tunnel_invocation_count(&log), 0, "no tunnel opens");
    assert_eq!(master_invocation_count(&log), 1, "one supervised master");
    assert_eq!(web_prep_invocation_count(&log), 1, "one preparation");
}

#[test]
fn remote_web_emits_prep_url_and_browser_only_after_tunnel_readiness() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let stdout_path = env.project_root.join("stdout.log");
    let tunnel_plan = env.project_root.join("tunnel.plan");
    let master_exit_plan = env.project_root.join("master-exit.plan");
    let tunnel_ready_gate = env.project_root.join("tunnel-ready");
    let master_exit_gate = env.project_root.join("master-exit");
    let (browser_bin, browser_log) = write_browser_shim(&env);
    let ambient_path = std::env::var_os("PATH").unwrap_or_default();
    let path = std::env::join_paths(
        std::iter::once(browser_bin.clone()).chain(std::env::split_paths(&ambient_path)),
    )
    .expect("browser PATH");
    std::fs::write(&tunnel_plan, "0\n").expect("write tunnel plan");
    std::fs::write(&master_exit_plan, "0\n").expect("write master exit plan");
    let stdout = std::fs::File::create(&stdout_path).expect("create stdout log");

    let mut child = remote_web_command(&env, &log)
        .env(
            "RIMZ_TEST_SSH_WEB_PREP_STDERR",
            "remote preparation started\n",
        )
        .env("RIMZ_TEST_SSH_TUNNEL_READY_GATE", &tunnel_ready_gate)
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &tunnel_plan)
        .env("RIMZ_TEST_SSH_MASTER_EXIT_GATE", &master_exit_gate)
        .env("RIMZ_TEST_SSH_MASTER_EXIT_PLAN", &master_exit_plan)
        .env("RIMZ_TEST_BROWSER_LOG", &browser_log)
        .env("PATH", path)
        .stdout(stdout)
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn remote web connect");

    wait_for_tunnel_invocation(&mut child, &log);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        std::fs::read_to_string(&stdout_path).unwrap_or_default(),
        "",
        "URL waits for local listener readiness"
    );
    assert!(!browser_log.exists(), "browser waits for tunnel readiness");

    std::fs::write(&tunnel_ready_gate, "").expect("release tunnel readiness");
    let stdout = wait_for_notify_log(&stdout_path, &["/?room=rimz-project-a1b2c3\n"]);
    let (port, url) = relay_url(&stdout);
    let browser = wait_for_notify_log(&browser_log, &[url]);
    assert_eq!(browser.trim(), url);
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        child.try_wait().expect("poll remote web connect").is_none(),
        "browser open must leave the ControlMaster-owned tunnel in the foreground"
    );

    std::fs::write(&master_exit_gate, "").expect("release master exit");
    let out = child.wait_with_output().expect("wait remote web connect");
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&stdout_path)
            .expect("read stdout")
            .trim(),
        url
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("remote preparation started"),
        "preparation stderr stays visible"
    );
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("test-web-token"),
        "the tunnel credential stays off stderr"
    );
    assert_eq!(tunnel_invocation_count(&log), 1);
    assert_ne!(
        tunnel_forward_ports(&log),
        [port],
        "SSH forwards through an ephemeral port behind the public relay"
    );
}

#[test]
fn remote_web_relay_injects_auth_for_http_and_safari_websockets() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let request_log = env.project_root.join("tunnel-http.log");
    let stdout_path = env.project_root.join("stdout.log");
    let stderr_path = env.project_root.join("stderr.log");
    let tunnel_exit_gate = env.project_root.join("tunnel-exit");
    let plan = env.project_root.join("tunnel.plan");
    std::fs::write(&plan, "0\n").expect("write tunnel plan");
    let stdout = std::fs::File::create(&stdout_path).expect("create stdout log");
    let stderr = std::fs::File::create(&stderr_path).expect("create stderr log");

    let mut child = remote_web_command(&env, &log)
        .arg("--no-reconnect")
        .env("RIMZ_TEST_SSH_TUNNEL_HTTP_LOG", &request_log)
        .env("RIMZ_TEST_SSH_TUNNEL_EXIT_GATE", &tunnel_exit_gate)
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &plan)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .expect("spawn remote web relay");

    let stdout = wait_for_web_url(&mut child, &stdout_path, &stderr_path, &log);
    let (port, _) = relay_url(&stdout);
    assert_ne!(tunnel_forward_ports(&log), [port]);

    let mut http = TcpStream::connect(("127.0.0.1", port)).expect("connect HTTP relay");
    http.write_all(b"GET / HTTP/1.1\r\nHost: local\r\nAuthorization: Bearer attacker\r\n\r\n")
        .expect("write HTTP request");
    let response = read_http_head(&mut http);
    assert!(response.starts_with(b"HTTP/1.1 204 No Content"));
    drop(http);

    let mut websocket = TcpStream::connect(("127.0.0.1", port)).expect("connect WS relay");
    websocket
        .write_all(b"GET /ws HTTP/1.1\r\nHost: local\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\nclient-frame")
        .expect("write Safari-shaped upgrade");
    websocket
        .shutdown(std::net::Shutdown::Write)
        .expect("finish client frames");
    let mut response = Vec::new();
    websocket
        .read_to_end(&mut response)
        .expect("read upgraded response");
    assert!(response.starts_with(b"HTTP/1.1 101 Switching Protocols"));
    assert!(response.ends_with(b"server-frame"));

    std::fs::write(&tunnel_exit_gate, "").expect("release tunnel exit");
    let out = wait_for_web_exit(child, &stdout_path, &stderr_path, &log);
    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let requests = std::fs::read_to_string(&request_log).expect("read tunnel requests");
    assert_eq!(
        requests
            .matches("Authorization: Basic cmltejp0ZXN0LXdlYi10b2tlbg==\r\n")
            .count(),
        2,
        "{requests}"
    );
    assert!(!requests.contains("Bearer attacker"), "{requests}");
    assert!(requests.contains("Upgrade: websocket\r\n"), "{requests}");
    assert!(requests.contains("client-frame"), "{requests}");
}

fn read_http_head(stream: &mut TcpStream) -> Vec<u8> {
    let mut head = Vec::new();
    let mut byte = [0_u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        stream.read_exact(&mut byte).expect("read HTTP response");
        head.push(byte[0]);
    }
    head
}

#[test]
fn remote_web_reconnects_once_after_established_transport_exit() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let tunnel_plan = env.project_root.join("tunnel.plan");
    let master_exit_plan = env.project_root.join("master-exit.plan");
    std::fs::write(&tunnel_plan, "0\n0\n").expect("write tunnel plan");
    std::fs::write(&master_exit_plan, "255\n0\n").expect("write master exit plan");

    let out = remote_web_command(&env, &log)
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &tunnel_plan)
        .env("RIMZ_TEST_SSH_MASTER_EXIT_MS", "500")
        .env("RIMZ_TEST_SSH_MASTER_EXIT_PLAN", &master_exit_plan)
        .bounded_output()
        .expect("run reconnecting remote web tunnel");

    assert!(
        out.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(tunnel_invocation_count(&log), 2);
    assert_eq!(master_invocation_count(&log), 2);
    assert_eq!(web_prep_invocation_count(&log), 2);
    assert!(String::from_utf8_lossy(&out.stderr).contains("rimz: tunnel to dev-box restored"));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("web tunnel to dev-box lost — reconnecting"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn remote_web_recovers_from_prep_transport_failure_after_confirmed_master() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let prep_plan = env.project_root.join("prep.plan");
    let tunnel_plan = env.project_root.join("tunnel.plan");
    std::fs::write(&prep_plan, "255\n0\n").expect("write prep plan");
    std::fs::write(&tunnel_plan, "0\n").expect("write tunnel plan");
    let out = remote_web_command(&env, &log)
        .env("RIMZ_TEST_SSH_WEB_PREP_PLAN", &prep_plan)
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &tunnel_plan)
        .env("RIMZ_TEST_SSH_MASTER_EXIT_MS", "500")
        .bounded_output()
        .expect("run prep retry");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(master_invocation_count(&log), 2);
    assert_eq!(web_prep_invocation_count(&log), 2);
    assert_eq!(tunnel_invocation_count(&log), 1);
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);
    assert!(String::from_utf8_lossy(&out.stderr).contains(
        "rimz: web tunnel to dev-box lost — reconnecting in the background; Ctrl-C stops"
    ));
}

#[test]
fn remote_web_retries_control_forward_exit_before_readiness() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let tunnel_plan = env.project_root.join("tunnel.plan");
    std::fs::write(&tunnel_plan, "255\n0\n").expect("write tunnel plan");
    let out = remote_web_command(&env, &log)
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &tunnel_plan)
        .env("RIMZ_TEST_SSH_MASTER_EXIT_MS", "500")
        .bounded_output()
        .expect("run forward retry");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(master_invocation_count(&log), 2);
    assert_eq!(web_prep_invocation_count(&log), 2);
    assert_eq!(tunnel_invocation_count(&log), 2);
    assert_eq!(String::from_utf8_lossy(&out.stdout).lines().count(), 1);
    assert!(String::from_utf8_lossy(&out.stderr).contains(
        "rimz: web tunnel to dev-box lost — reconnecting in the background; Ctrl-C stops"
    ));
}

#[test]
fn remote_web_no_reconnect_stays_a_direct_one_shot() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let stdout_path = env.project_root.join("stdout.log");
    let stderr_path = env.project_root.join("stderr.log");
    let tunnel_exit_gate = env.project_root.join("tunnel-exit");
    let plan = env.project_root.join("tunnel.plan");
    std::fs::write(&plan, "255\n0\n").expect("write tunnel plan");
    let stdout = std::fs::File::create(&stdout_path).expect("create stdout log");
    let stderr = std::fs::File::create(&stderr_path).expect("create stderr log");

    let mut child = remote_web_command(&env, &log)
        .arg("--no-reconnect")
        .env("RIMZ_TEST_SSH_TUNNEL_LISTEN", "1")
        .env("RIMZ_TEST_SSH_TUNNEL_EXIT_GATE", &tunnel_exit_gate)
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &plan)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
        .expect("spawn one-shot remote web tunnel");

    let stdout = wait_for_web_url(&mut child, &stdout_path, &stderr_path, &log);
    relay_url(&stdout);
    std::fs::write(&tunnel_exit_gate, "").expect("release tunnel exit");
    let out = wait_for_web_exit(child, &stdout_path, &stderr_path, &log);

    assert!(!out.status.success());
    assert_eq!(master_invocation_count(&log), 0);
    assert_eq!(web_prep_invocation_count(&log), 1);
    assert_eq!(tunnel_invocation_count(&log), 1);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("exited with status 255; not reconnecting"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn remote_web_fatal_exit_before_readiness_emits_no_url() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("tunnel.plan");
    std::fs::write(&plan, "2\n").expect("write tunnel plan");

    let out = remote_web_command(&env, &log)
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &plan)
        .bounded_output()
        .expect("run fatal remote web tunnel");

    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "URL must not precede readiness");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("exited with status 2; not reconnecting"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(tunnel_invocation_count(&log), 1);
}

#[test]
fn remote_web_direct_clean_exit_before_readiness_is_an_error() {
    let env = Env::new();
    let log = env.project_root.join("ssh-trace.log");
    let plan = env.project_root.join("tunnel.plan");
    std::fs::write(&plan, "0\n").expect("write tunnel plan");

    let out = remote_web_command(&env, &log)
        .arg("--no-reconnect")
        .env("RIMZ_TEST_SSH_TUNNEL_PLAN", &plan)
        .bounded_output()
        .expect("run clean remote web tunnel exit before readiness");

    assert!(!out.status.success());
    assert!(out.stdout.is_empty(), "URL must not precede readiness");
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("web tunnel exited before local port accepted connections"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(tunnel_invocation_count(&log), 1);
    assert_eq!(master_invocation_count(&log), 0);
}

#[test]
fn remote_alias_update_drives_connect_and_no_resume() {
    let env = Env::new();

    let add = env
        .rimz()
        .args(["remote", "add", "prod", "agent@prod-box:query-engine"])
        .bounded_output()
        .expect("run rimz remote add");
    assert!(
        add.status.success(),
        "add succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&add.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&add.stdout),
        "saved prod -> agent@prod-box:query-engine; connect: rimz remote connect prod\n"
    );

    let update = env
        .rimz()
        .args(["remote", "update", "prod", "agent@prod-box:other-engine"])
        .bounded_output()
        .expect("run rimz remote update");
    assert!(
        update.status.success(),
        "update succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&update.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&update.stdout),
        "updated prod -> agent@prod-box:other-engine\n"
    );

    let printed = env
        .rimz()
        .args(["remote", "connect", "prod", "--print"])
        .env("TERM", "xterm-256color")
        .bounded_output()
        .expect("run rimz remote connect alias --print");
    let line = stdout_line(&printed);
    assert!(
        line.contains("agent@prod-box"),
        "alias target rides into ssh: {line}"
    );
    assert!(
        line.contains("other-engine"),
        "alias session rides into remote rimz: {line}"
    );

    let reset = env
        .rimz()
        .args(["remote", "connect", "prod", "--no-resume", "--print"])
        .env("TERM", "xterm-256color")
        .bounded_output()
        .expect("run rimz remote connect --no-resume --print");
    let line = stdout_line(&reset);
    assert!(
        line.contains("--no-resume"),
        "remote connect --no-resume injects --no-resume: {line}"
    );
}
