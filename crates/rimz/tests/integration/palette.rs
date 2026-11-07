//! Palette entry guards without a terminal or a live multiplexer.

use crate::common::Env;

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
    printf 'human|%%7|1|attached,focused\n' ;;
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
