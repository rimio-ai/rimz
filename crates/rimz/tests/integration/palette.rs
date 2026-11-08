//! Palette guards and printed room commands without a live multiplexer.

use std::num::NonZeroU16;
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::common::Env;
use crate::common::ssh_trace::{Answers, FakeTerminal};

#[track_caller]
fn palette_footer(env: &Env, query: &str) -> String {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 24,
            cols: 120,
            ..Default::default()
        })
        .unwrap();
    let session = env.resolve_workspace(&env.project_root).session_name;
    let mut command = CommandBuilder::new(env.rimz_bin());
    env.pin_pty_command(&mut command);
    command.cwd(&env.home_root);
    command.args([
        "palette",
        "--mode",
        "commands",
        "--session-name",
        &session,
        "--mux",
        "tmux",
    ]);
    let mut child = pair.slave.spawn_command(command).unwrap();
    drop(pair.slave);
    let terminal = FakeTerminal::new(pair.master, Duration::ZERO, Answers::StatusOnly);
    let mut footer = String::new();
    let mut cursor_replies = 0;
    for input in ["", query] {
        terminal.write(input.as_bytes());
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let output = terminal.output();
            let cursor_queries = output
                .windows(4)
                .filter(|bytes| *bytes == b"\x1b[6n")
                .count();
            while cursor_replies < cursor_queries {
                terminal.write(b"\x1b[1;1R");
                cursor_replies += 1;
            }
            let mut parser = vt100::Parser::new(
                NonZeroU16::new(24).unwrap(),
                NonZeroU16::new(120).unwrap(),
                0,
            );
            parser.process(&output);
            let screen = parser.screen().contents();
            let heading = format!("palette: >{input}");
            footer = screen
                .lines()
                .next_back()
                .unwrap_or_default()
                .trim_end()
                .to_owned();
            let prefix = if input.is_empty() {
                "$ rimz agents list".to_owned()
            } else {
                format!("$ rimz {input} --session-name")
            };
            if screen.lines().next().map(str::trim_end) == Some(heading.as_str())
                && (footer == "nothing to run" || footer.starts_with(&prefix))
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "palette did not settle for {input:?}, footer {footer:?}: {screen}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    terminal.write(b"\x1b");
    assert!(child.wait().unwrap().success());
    terminal.finish();
    footer
}

#[test]
fn printed_sidebar_navigation_resolves_the_palettes_named_room_off_room() {
    let env = Env::new();
    env.record(&env.project_root);
    let session = env.resolve_workspace(&env.project_root).session_name;
    let mut actual = Vec::new();
    for verb in ["walk next", "walk prev", "slot 9", "last"] {
        let footer = palette_footer(&env, &format!("sidebar {verb}"));
        actual.push(footer);
    }
    assert_eq!(
        actual,
        ["walk next", "walk prev", "slot 9", "last"]
            .map(|verb| format!("$ rimz sidebar {verb} --session-name {session}"))
    );
    for footer in actual {
        let args = shlex::split(footer.trim_start_matches("$ ")).unwrap();
        let output = env
            .rimz()
            .current_dir(&env.home_root)
            .args(["--mux", "tmux"])
            .args(&args[1..])
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(output.stdout.is_empty(), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains("rimz sidebar repair"), "{footer}: {stderr}");
        assert!(!stderr.contains("no room"), "{footer}: {stderr}");
    }
}

#[cfg(target_os = "linux")]
struct PopupFixture {
    env: Env,
    bin: std::path::PathBuf,
    trace: std::path::PathBuf,
    during: std::path::PathBuf,
}

#[cfg(target_os = "linux")]
impl PopupFixture {
    fn new() -> Self {
        let env = Env::new();
        env.record(&env.project_root);
        let bin = env.project_root.join("bin");
        crate::common::write_path_shim(
            &bin,
            "tmux",
            r#"if [ "$1" = -S ]; then shift 3; fi
printf '%s\n' "$1" >> "$RIMZ_TEST_POPUP_TRACE"
case "$1" in
  -V) printf 'tmux 3.8\n' ;;
  list-clients)
    if [ "$RIMZ_TEST_POPUP_VIEW_FAIL" = 1 ]; then exit 1; fi
    printf 'human|%%7|1|root||attached,focused\n' ;;
  display-popup)
    if [ -f "$RIMZ_TEST_POPUP_TYPING" ]; then
      cp "$RIMZ_TEST_POPUP_TYPING" "$RIMZ_TEST_POPUP_DURING"
    fi
    sleep 0.025
    if [ "$RIMZ_TEST_POPUP_FAIL" = 1 ]; then printf 'popup refused\n' >&2; fi
    exit "${RIMZ_TEST_POPUP_FAIL:-0}" ;;
esac"#,
        );
        Self {
            trace: env.project_root.join("popup.trace"),
            during: env.project_root.join("popup.during.json"),
            env,
            bin,
        }
    }

    fn command(&self) -> std::process::Command {
        let mut command = self.env.rimz();
        command
            .args(["palette", "--popup", "--client", "human", "--mux", "tmux"])
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap()),
            )
            .env("RIMZ_TEST_POPUP_TRACE", &self.trace)
            .env("RIMZ_TEST_POPUP_DURING", &self.during)
            .env("RIMZ_TEST_POPUP_TYPING", self.typing_path());
        command
    }

    fn typing_path(&self) -> std::path::PathBuf {
        self.env.runtime_paths().lane_path("typing.json")
    }

    fn quiet(path: &std::path::Path) -> Option<u64> {
        let bytes = std::fs::read(path).ok()?;
        let record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        record["panes"]["tmux:%7"]["quiet_until_ms"].as_u64()
    }
}

#[cfg(target_os = "linux")]
#[test]
fn popup_stamps_its_clients_pane_before_and_after_each_backend_result() {
    let mut actual = Vec::new();
    for fail in [false, true] {
        let fixture = PopupFixture::new();
        let output = fixture
            .command()
            .env("RIMZ_TEST_POPUP_FAIL", if fail { "1" } else { "0" })
            .output()
            .unwrap();
        let during = PopupFixture::quiet(&fixture.during);
        let after = PopupFixture::quiet(&fixture.typing_path());
        actual.push((
            fail,
            output.status.success(),
            during.is_some(),
            after
                .zip(during)
                .is_some_and(|(after, during)| after > during),
        ));
    }
    assert_eq!(
        actual,
        [(false, true, true, true), (true, false, true, true)]
    );
}

#[cfg(target_os = "linux")]
#[test]
fn popup_still_opens_when_its_typing_stamp_or_client_view_fails() {
    let mut actual = Vec::new();
    for failure in ["stamp", "view"] {
        let fixture = PopupFixture::new();
        let lock = fixture.env.runtime_paths().locks_dir.join("typing.lock");
        let _guard =
            (failure == "stamp").then(|| rimz::disk::lock::WorkspaceLock::acquire(&lock).unwrap());
        let output = fixture
            .command()
            .env(
                "RIMZ_TEST_POPUP_VIEW_FAIL",
                if failure == "view" { "1" } else { "0" },
            )
            .output()
            .unwrap();
        let trace = std::fs::read_to_string(&fixture.trace).unwrap();
        actual.push((
            failure,
            output.status.success(),
            trace.lines().any(|line| line == "list-clients"),
            trace.lines().any(|line| line == "display-popup"),
            fixture.typing_path().exists(),
        ));
    }
    assert_eq!(
        actual,
        [
            ("stamp", true, true, true, false),
            ("view", true, true, true, false)
        ]
    );
}

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
