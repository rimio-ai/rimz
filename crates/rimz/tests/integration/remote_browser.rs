//! The laptop browser relay end to end: the room's BROWSER helper against real
//! ingests, its fallbacks, and the laptop's open of a relayed line.

use std::io::Read as _;
#[cfg(target_os = "linux")]
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

#[cfg(target_os = "linux")]
use crate::common::CommandTimeoutExt;
use crate::common::Env;

const URL: &str = "https://github.com/rimio-ai/rimz/pull/1967?token=secret";

#[cfg(target_os = "linux")]
struct Ingest {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    seq: u64,
}

#[cfg(target_os = "linux")]
impl Ingest {
    fn spawn(env: &Env, client: &str) -> Self {
        let dir = env.project_root.to_string_lossy().into_owned();
        let mut child = env
            .rimz()
            .args(["remote", "link-stats", "ingest", "--dir", &dir])
            .env("SSH_CONNECTION", client)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn link-stats ingest");
        let stdin = child.stdin.take().expect("ingest stdin");
        let stdout = BufReader::new(child.stdout.take().expect("ingest stdout"));
        Self {
            child,
            stdin,
            stdout,
            seq: 0,
        }
    }

    /// Send one probe and return the line the ingest writes next.
    fn probe(&mut self, opens_urls: bool, idle_ms: Option<u64>) -> serde_json::Value {
        self.probe_every(opens_urls, idle_ms, None)
    }

    /// [`Self::probe`] from a laptop that names its probe interval.
    fn probe_every(
        &mut self,
        opens_urls: bool,
        idle_ms: Option<u64>,
        interval_ms: Option<u64>,
    ) -> serde_json::Value {
        self.seq += 1;
        let mut probe = serde_json::json!({
            "v": "rimz.link.v1", "seq": self.seq, "sent_at_ms": 1_000u64,
            "stats": {"miss_pct": 0, "window": 0},
        });
        if opens_urls {
            probe["opens_urls"] = true.into();
        }
        if let Some(idle_ms) = idle_ms {
            probe["idle_ms"] = idle_ms.into();
        }
        if let Some(interval_ms) = interval_ms {
            probe["interval_ms"] = interval_ms.into();
        }
        writeln!(self.stdin, "{probe}").expect("write probe");
        self.next_line()
    }

    fn next_line(&mut self) -> serde_json::Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read ingest line");
        serde_json::from_str(&line).unwrap_or_else(|_| panic!("ingest line {line:?}"))
    }

    fn finish(self) {
        let Self {
            mut child, stdin, ..
        } = self;
        drop(stdin);
        let status = child.wait().expect("wait ingest");
        assert!(status.success(), "ingest exits cleanly: {status:?}");
    }
}

/// A recording opener: appends its URL and the BROWSER it sees to <dir>/<name>.log.
fn recording_opener(dir: &Path, name: &str) -> PathBuf {
    exiting_opener(dir, name, 0)
}

/// A recording opener that says its name on stdout and exits with `code`.
fn exiting_opener(dir: &Path, name: &str, code: u8) -> PathBuf {
    let log = dir.join(format!("{name}.log"));
    crate::common::write_path_shim(
        dir,
        name,
        &format!(
            "printf '%s|%s\\n' \"$1\" \"${{BROWSER-unset}}\" >> '{}'\necho 'opened by {name}'\nexit {code}",
            log.display()
        ),
    )
}

fn opener_log(dir: &Path, name: &str) -> Vec<String> {
    std::fs::read_to_string(dir.join(format!("{name}.log")))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// The opener's log once it holds `count` lines: a detached opener writes after
/// the helper has returned.
#[cfg(target_os = "linux")]
fn wait_for_opener(dir: &Path, name: &str, count: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let lines = opener_log(dir, name);
        if lines.len() >= count || Instant::now() >= deadline {
            return lines;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// The room's helper path, which the room writes into its state directory.
#[cfg(target_os = "linux")]
fn helper_path(env: &Env) -> PathBuf {
    env.state_path_for(&env.project_root).browser_helper
}

/// The helper as a room pane runs it: pinned to the room, with no controlling
/// terminal (util-linux `setsid`), and xdg-open resolving to a recording stub in bin.
#[cfg(target_os = "linux")]
fn open_url(env: &Env, bin: &Path, url: &str) -> Command {
    let rimz = env.rimz();
    let mut cmd = Command::new("setsid");
    cmd.arg("--wait").arg(rimz.get_program());
    for (key, value) in rimz.get_envs() {
        match value {
            Some(value) => cmd.env(key, value),
            None => cmd.env_remove(key),
        };
    }
    if let Some(dir) = rimz.get_current_dir() {
        cmd.current_dir(dir);
    }
    cmd.args(rimz.get_args())
        .args(["remote", "open-url", url])
        .envs(rimz::workspace::pin_env(
            &env.workspace_id,
            &env.project_root,
        ))
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("BROWSER", helper_path(env))
        .env_remove("SSH_CONNECTION")
        .env_remove("SSH_TTY")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null());
    cmd
}

#[cfg(target_os = "linux")]
fn open_sockets(env: &Env) -> Vec<String> {
    let mut names = std::fs::read_dir(&env.runtime_paths().sock_dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .filter(|name| rimz::remote::browser::socket_pid(name).is_some())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[cfg(target_os = "linux")]
fn dead_pid() -> u32 {
    let mut child = Command::new("true").spawn().expect("spawn true");
    let pid = child.id();
    child.wait().expect("reap true");
    pid
}

#[cfg(target_os = "linux")]
#[test]
fn open_helper_relays_a_url_to_the_most_recently_active_link_once() {
    let env = Env::new();
    env.record(&env.project_root);
    let bin = env.home_root.join("open-bin");
    recording_opener(&bin, "xdg-open");
    let runtime = env.runtime_paths();
    runtime.ensure_runtime_dirs().expect("runtime dirs");
    let stale = rimz::remote::browser::socket_path(&runtime, dead_pid()).expect("stale path");
    drop(std::os::unix::net::UnixListener::bind(&stale).expect("bind stale socket"));
    assert!(stale.exists(), "a killed ingest leaves its socket file");

    let mut old_laptop = Ingest::spawn(&env, "old-laptop");
    assert_eq!(old_laptop.probe(false, None)["seq"], 1);
    assert_eq!(
        open_sockets(&env).len(),
        1,
        "an ingest listens only for a laptop that opens URLs"
    );
    let mut idle = Ingest::spawn(&env, "idle-laptop");
    assert_eq!(idle.probe(true, Some(600_000))["seq"], 1);
    let mut active = Ingest::spawn(&env, "active-laptop");
    assert_eq!(active.probe(true, Some(0))["seq"], 1);
    assert!(
        !stale.exists(),
        "a listening ingest sweeps a dead ingest's socket"
    );
    assert_eq!(open_sockets(&env).len(), 2, "{:?}", open_sockets(&env));

    let out = open_url(&env, &bin, URL)
        .env("SSH_CONNECTION", "agent-pane")
        .bounded_output()
        .expect("run open helper");
    assert!(
        out.status.success(),
        "relay succeeds\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        active.next_line(),
        serde_json::json!({"v": "rimz.link.v1", "open": URL}),
        "the most recently active laptop gets the URL"
    );
    assert_eq!(
        active.probe(true, Some(0))["seq"],
        2,
        "one open line, then acks"
    );
    assert_eq!(
        idle.probe(true, Some(600_000))["seq"],
        2,
        "the idle laptop got nothing"
    );
    assert_eq!(old_laptop.probe(false, None)["seq"], 2);

    let helpers = [URL, "https://a.test/second"].map(|url| {
        open_url(&env, &bin, url)
            .env("SSH_CONNECTION", "agent-pane")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn open helper")
    });
    for mut helper in helpers {
        assert!(helper.wait().expect("wait open helper").success());
    }
    let mut opened = [active.next_line(), active.next_line()].map(|line| line["open"].clone());
    opened.sort_by_key(ToString::to_string);
    assert_eq!(
        opened,
        ["https://a.test/second", URL].map(serde_json::Value::from),
        "two helpers at once are both served"
    );
    assert_eq!(active.probe(true, Some(0))["seq"], 3);
    assert_eq!(idle.probe(true, Some(600_000))["seq"], 3);
    assert_eq!(old_laptop.probe(false, None)["seq"], 3);
    assert!(
        opener_log(&bin, "xdg-open").is_empty(),
        "a relay opens nothing on the host"
    );

    let forged = format!(
        "https://a.test/\n{}",
        serde_json::json!({"v": "rimz.link.v1", "open": "https://evil.test/"})
    );
    let out = open_url(&env, &bin, &forged)
        .env("SSH_CONNECTION", "agent-pane")
        .bounded_output()
        .expect("run open helper with a forged line");
    assert!(!out.status.success(), "a URL with a newline opens nowhere");
    assert_eq!(
        active.probe(true, Some(0))["seq"],
        4,
        "no forged line crossed the link"
    );
    assert_eq!(idle.probe(true, Some(600_000))["seq"], 4);

    for ingest in [old_laptop, idle, active] {
        ingest.finish();
    }
    assert!(
        open_sockets(&env).is_empty(),
        "a clean stream end removes its socket"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn open_helper_fails_over_to_the_next_link_when_one_cannot_deliver() {
    let env = Env::new();
    env.record(&env.project_root);
    let bin = env.home_root.join("open-bin");
    recording_opener(&bin, "xdg-open");
    let mut gone = Ingest::spawn(&env, "gone-laptop");
    assert_eq!(gone.probe(true, Some(0))["seq"], 1);
    let mut next = Ingest::spawn(&env, "next-laptop");
    assert_eq!(next.probe(true, Some(600_000))["seq"], 1);
    let Ingest {
        mut child,
        stdin,
        stdout,
        ..
    } = gone;
    drop(stdout);

    let out = open_url(&env, &bin, URL)
        .env("SSH_CONNECTION", "agent-pane")
        .bounded_output()
        .expect("run open helper");
    assert!(
        out.status.success(),
        "the next link takes the URL\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        next.next_line(),
        serde_json::json!({"v": "rimz.link.v1", "open": URL}),
        "a link that cannot write its stream hands the URL on"
    );
    assert_eq!(next.probe(true, Some(600_000))["seq"], 2, "one open line");
    assert!(opener_log(&bin, "xdg-open").is_empty());
    drop(stdin);
    let _ = child.wait();
    next.finish();
}

#[cfg(target_os = "linux")]
#[test]
fn open_helper_skips_a_link_whose_probes_stopped() {
    let env = Env::new();
    env.record(&env.project_root);
    let bin = env.home_root.join("open-bin");
    recording_opener(&bin, "xdg-open");
    let mut asleep = Ingest::spawn(&env, "asleep-laptop");
    assert_eq!(asleep.probe_every(true, Some(0), Some(100))["seq"], 1);
    let mut fresh = Ingest::spawn(&env, "fresh-laptop");
    assert_eq!(fresh.probe(true, Some(600_000))["seq"], 1);
    std::thread::sleep(Duration::from_millis(500));

    let out = open_url(&env, &bin, URL)
        .env("SSH_CONNECTION", "agent-pane")
        .bounded_output()
        .expect("run open helper");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        asleep.probe_every(true, Some(0), Some(100))["seq"],
        2,
        "the silent link got nothing"
    );
    assert_eq!(
        fresh.next_line(),
        serde_json::json!({"v": "rimz.link.v1", "open": URL}),
        "a link silent past three probe intervals loses the URL to a live one"
    );
    fresh.finish();

    std::thread::sleep(Duration::from_millis(500));
    let out = open_url(&env, &bin, URL)
        .env("SSH_CONNECTION", "agent-pane")
        .bounded_output()
        .expect("run open helper with only a silent link");
    assert!(
        !out.status.success(),
        "a silent link is no delivery, and nothing else can open"
    );
    assert_eq!(asleep.probe_every(true, Some(0), Some(100))["seq"], 3);
    asleep.finish();
}

#[cfg(target_os = "linux")]
#[test]
fn open_helper_relays_again_once_a_reset_removed_the_links_socket() {
    let env = Env::new();
    env.record(&env.project_root);
    let bin = env.home_root.join("open-bin");
    recording_opener(&bin, "xdg-open");
    let mut ingest = Ingest::spawn(&env, "laptop");
    assert_eq!(ingest.probe(true, Some(0))["seq"], 1);
    let runtime = env.runtime_paths();
    std::fs::remove_dir_all(&runtime.sock_dir).expect("remove sock dir as a reset does");
    runtime.ensure_runtime_dirs().expect("reborn runtime dirs");
    let deadline = Instant::now() + Duration::from_secs(10);
    while open_sockets(&env).is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(open_sockets(&env).len(), 1, "the ingest listens again");

    let out = open_url(&env, &bin, URL)
        .env("SSH_CONNECTION", "agent-pane")
        .bounded_output()
        .expect("run open helper after a reset");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        ingest.next_line(),
        serde_json::json!({"v": "rimz.link.v1", "open": URL})
    );
    ingest.finish();
}

#[cfg(target_os = "linux")]
#[test]
fn open_helper_opens_locally_without_reaching_itself_and_fails_with_nothing_left() {
    let env = Env::new();
    env.record(&env.project_root);
    let bin = env.home_root.join("open-bin");
    recording_opener(&bin, "xdg-open");
    let user = recording_opener(&bin, "user-browser");
    let spaced = recording_opener(&env.home_root.join("my apps"), "spaced-browser");
    let open = |saved: &str, url: &str| {
        let out = open_url(&env, &bin, url)
            .env("RIMZ_USER_BROWSER", saved)
            .bounded_output()
            .expect("run open helper");
        assert!(
            out.status.success(),
            "{saved}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };

    open(&user.display().to_string(), URL);
    assert_eq!(
        opener_log(&bin, "user-browser"),
        [format!("{URL}|{}", user.display())],
        "the user's browser opens the URL and sees itself as BROWSER"
    );

    open("", "vscode://file/tmp/x");
    assert_eq!(
        wait_for_opener(&bin, "xdg-open", 1),
        ["vscode://file/tmp/x|unset"],
        "a local open passes any scheme and never hands the opener the helper"
    );

    open(&helper_path(&env).display().to_string(), URL);
    assert_eq!(
        wait_for_opener(&bin, "xdg-open", 2)[1..],
        [format!("{URL}|unset")],
        "a saved helper is never run, nor handed to the opener as BROWSER"
    );

    open("/nonexistent/browser", URL);
    assert_eq!(
        wait_for_opener(&bin, "xdg-open", 3)[2..],
        [format!("{URL}|/nonexistent/browser")],
        "a saved browser that fails to start falls through to the platform opener"
    );

    open(&format!("/nonexistent/browser:{}", user.display()), URL);
    assert_eq!(
        opener_log(&bin, "user-browser")[1..],
        [format!("{URL}|/nonexistent/browser:{}", user.display())],
        "an entry that fails to start hands the URL to the next entry"
    );

    open(&spaced.display().to_string(), URL);
    assert_eq!(
        opener_log(&env.home_root.join("my apps"), "spaced-browser"),
        [format!("{URL}|{}", spaced.display())],
        "a saved program path with a space in it runs as one program"
    );

    let out = open_url(&env, &bin, URL)
        .env("RIMZ_USER_BROWSER", &user)
        .env("SSH_CONNECTION", "agent-pane")
        .bounded_output()
        .expect("run open helper with nothing to open it");
    assert!(
        !out.status.success(),
        "no link, no display, and no terminal is a failed open"
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(
        opener_log(&bin, "user-browser").len(),
        2,
        "a headless SSH session never runs the saved browser"
    );
    assert_eq!(opener_log(&bin, "xdg-open").len(), 3, "nothing opened");
}

#[cfg(target_os = "linux")]
#[test]
fn open_helper_becomes_the_saved_browser_where_a_page_can_show() {
    let env = Env::new();
    env.record(&env.project_root);
    let bin = env.home_root.join("open-bin");
    recording_opener(&bin, "xdg-open");
    for (name, code) in [("user-browser", 0), ("failing-browser", 3)] {
        let opener = exiting_opener(&bin, name, code);
        let out = open_url(&env, &bin, URL)
            .env("RIMZ_USER_BROWSER", &opener)
            .bounded_output()
            .expect("run open helper with a saved browser");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(i32::from(code)), "{stderr}");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("opened by {name}\n"),
            "the caller reads the user's browser's own stdout"
        );
        assert!(
            !stderr.contains("rimz:"),
            "the helper adds nothing: {stderr}"
        );
        assert_eq!(
            opener_log(&bin, name),
            [format!("{URL}|{}", opener.display())],
            "the user's browser opens the URL and sees itself as BROWSER"
        );
    }
    assert!(
        opener_log(&bin, "xdg-open").is_empty(),
        "an exit status, even a failing one, is the user's browser's answer"
    );
}

#[test]
fn open_helper_copies_the_url_to_the_terminal_clipboard_without_a_link() {
    use portable_pty::CommandBuilder;

    let env = Env::new();
    env.record(&env.project_root);
    let bin = env.home_root.join("open-bin");
    let user = recording_opener(&bin, "user-browser");
    let pair = crate::common::ssh_trace::remote_connect_pty();
    let mut cmd = CommandBuilder::new(crate::common::cargo_bin("rimz", env!("CARGO_BIN_EXE_rimz")));
    env.pin_pty_command(&mut cmd);
    cmd.args(["remote", "open-url", URL]);
    for (key, value) in rimz::workspace::pin_env(&env.workspace_id, &env.project_root) {
        cmd.env(key, value);
    }
    cmd.env("SSH_CONNECTION", "agent-pane");
    cmd.env("RIMZ_USER_BROWSER", &user);
    cmd.env_remove("DISPLAY");
    cmd.env_remove("WAYLAND_DISPLAY");
    cmd.cwd(&env.project_root);
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .expect("spawn helper on a pty");
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().expect("pty reader");
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 4096];
        while let Ok(read @ 1..) = reader.read(&mut chunk) {
            bytes.extend_from_slice(&chunk[..read]);
        }
        bytes
    });
    let status = child.wait().expect("wait helper");
    drop(pair.master);
    let output = output.join().expect("pty output");
    assert!(status.success(), "{}", String::from_utf8_lossy(&output));
    let url = rimz::remote::browser::web_url(URL).unwrap();
    let sequence = rimz::remote::browser::clipboard_sequence(&url);
    assert!(
        output
            .windows(sequence.len())
            .any(|window| window == sequence),
        "{}",
        String::from_utf8_lossy(&output)
    );
    assert!(
        opener_log(&bin, "user-browser").is_empty(),
        "a headless SSH session copies the URL instead of running the saved browser"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn supervised_connect_opens_a_relayed_url_once_and_records_the_assist() {
    for opener in [true, false] {
        let env = Env::new();
        let log = env.project_root.join("ssh-trace.log");
        let bin = env.home_root.join("open-bin");
        std::fs::create_dir_all(&bin).expect("mkdir opener bin");
        if opener {
            recording_opener(&bin, "xdg-open");
        }
        let out = super::remote_connect_command(&env, &log)
            .env("PATH", &bin)
            .env_remove("SSH_TTY")
            .env("RIMZ_REMOTE_PROBE_MS", "10")
            .env(
                "RIMZ_TEST_PROBE_OPEN_URLS",
                format!("file:///etc/passwd {URL}"),
            )
            .env("RIMZ_TEST_WAIT_FOR_PROBE_MS", "500")
            .env("RIMZ_TEST_SSH_SLEEP_MS", "500")
            .bounded_output()
            .expect("run supervised connect");
        assert!(
            out.status.success(),
            "an open line never ends the link\nstderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
        if opener {
            assert_eq!(
                wait_for_opener(&bin, "xdg-open", 1),
                [format!("{URL}|unset")],
                "the laptop opens the http(s) URL once and refuses the file URL"
            );
        }
        let assists = std::fs::read_to_string(env.rimz_home().join("logs/assists.log.jsonl"))
            .unwrap_or_default();
        assert!(
            !assists.contains("token=secret") && !assists.contains("/pull/"),
            "{assists}"
        );
        let assists = assists
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        if !opener {
            assert!(
                assists.is_empty(),
                "a laptop with no opener never offers its browser: {assists:?}"
            );
            continue;
        }
        assert_eq!(
            assists.len(),
            1,
            "one assist per relayed URL tried: {assists:?}"
        );
        assert_eq!(assists[0]["assist"], "link_open");
        assert_eq!(assists[0]["host"], "dev-box");
        assert_eq!(assists[0]["scheme"], "https");
        assert_eq!(assists[0]["url_host"], "github.com");
        assert!(assists[0].get("error").is_none(), "{assists:?}");
    }
}
