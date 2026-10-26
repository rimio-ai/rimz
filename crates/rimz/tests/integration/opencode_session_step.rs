use super::*;
use base64::Engine as _;
use serde_json::{Value, json};
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

#[path = "opencode_session_wrapper.rs"]
mod wrapper;

const CATALOG: &str =
    r#"{"location":{},"data":[{"providerID":"opencode-go","id":"deepseek-v4.1-flash"}]}"#;

fn fake_opencode(env: &Env) -> PathBuf {
    let dir = env.home_root.join("opencode-bin");
    crate::common::write_path_shim(
        &dir,
        "opencode",
        r#"
if [ "${1:-}" = --version ]; then echo 2.1.4; exit 0; fi
if [ "${1:-}" = serve ]; then
  printf '%s\n' "$$" >> "$CHECK_LOG/serve-pids"
  sleep 30 &
  printf '%s\n' "$!" >> "$CHECK_LOG/serve-workers"
  env > "$CHECK_LOG/helper-env"
  printf '{"url":"%s"}\n' "$CHECK_URL"
  while IFS= read -r line; do :; done
  printf 'closed\n' > "$CHECK_LOG/closed"
  exit 0
fi
printf '%s\n' "$$" > "$CHECK_LOG/final-pid"
printf '%s\000' "$@" > "$CHECK_LOG/final-argv"
env > "$CHECK_LOG/final-env"
if [ -e "$CHECK_LOG/serve-pids" ]; then
  while IFS= read -r pid; do
    if kill -0 "$pid" 2>/dev/null; then printf 'alive\n' > "$CHECK_LOG/helper-alive"; fi
  done < "$CHECK_LOG/serve-pids"
fi
"#,
    );
    dir
}

fn step_command(
    env: &Env,
    origin: &str,
    model: &str,
    session: Option<&str>,
) -> std::process::Command {
    let dir = fake_opencode(env);
    let mut command = env.rimz();
    command.args(["agents", "session-step", "opencode", "--model", model]);
    if let Some(session) = session {
        command.args(["--session", session]);
    }
    command.arg("--").arg(dir.join("opencode"));
    if let Some(session) = session {
        command.args(["--session", session]);
    }
    command
        .args(["--standalone", "--prompt", "a task; $(not a command)"])
        .env("CHECK_URL", origin)
        .env("CHECK_LOG", &env.home_root)
        .env("PATH", path_with_front(&dir))
        .env("RIMZ_LAUNCH_REMINDERS", "keep for the TUI")
        .env("RIMZ_AGENT_MODEL", "not-the-requested-model")
        .env("OPENCODE_CONFIG_CONTENT", "{\"theme\":\"dark\"}");
    command
}

fn request_parts(request: &str) -> (&str, Value) {
    let (head, body) = request.split_once("\r\n\r\n").unwrap();
    let target = head
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap();
    let body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(body).unwrap()
    };
    (target, body)
}

fn final_args(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join("final-argv"))
        .unwrap()
        .split_terminator('\0')
        .map(str::to_owned)
        .collect()
}

#[test]
fn hidden_step_creates_or_switches_then_execs_without_changing_environment() {
    for session in [None, Some("ses_existing")] {
        let env = Env::new();
        let route = if session.is_some() {
            "POST /api/session/ses_existing/model"
        } else {
            "POST /api/session "
        };
        let (origin, server) = crate::oauth_usage::serve_http_routes(
            vec![
                ("GET /api/model?", 200, CATALOG.into()),
                (route, 204, String::new()),
            ],
            2,
        );
        let mut command = step_command(&env, &origin, "deepseek-v4.1-flash", session);
        let child = command
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read_to_string(env.home_root.join("final-pid"))
                .unwrap()
                .trim(),
            pid.to_string()
        );
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 2, "{requests:?}");
        let (target, _) = request_parts(&requests[0]);
        let url = url::Url::parse(&format!("{origin}{target}")).unwrap();
        assert_eq!(
            url.query_pairs().collect::<Vec<_>>(),
            [(
                "location[directory]".into(),
                env.project_root.to_str().unwrap().into()
            )]
        );
        let (_, body) = request_parts(&requests[1]);
        assert_eq!(
            body["model"],
            json!({"providerID":"opencode-go", "id":"deepseek-v4.1-flash"})
        );
        let id = session.unwrap_or_else(|| body["id"].as_str().unwrap());
        assert!(id.starts_with("ses_"));
        if session.is_none() {
            assert_eq!(id.len(), 30);
            assert_eq!(
                body["location"]["directory"],
                env.project_root.to_str().unwrap()
            );
        }
        assert_eq!(
            final_args(&env.home_root),
            [
                "--session",
                id,
                "--standalone",
                "--prompt",
                "a task; $(not a command)"
            ]
        );
        assert!(!env.home_root.join("helper-alive").exists());
        assert_eq!(
            std::fs::read_to_string(env.home_root.join("closed")).unwrap(),
            "closed\n"
        );
        // A worker in the helper's group dies with it, though the helper exited cleanly.
        let worker: u32 = std::fs::read_to_string(env.home_root.join("serve-workers"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while rimz::proc::process_is_live(worker, None) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!rimz::proc::process_is_live(worker, None));
        let helper_env = std::fs::read_to_string(env.home_root.join("helper-env")).unwrap();
        assert!(
            !helper_env
                .lines()
                .any(|line| line.starts_with("RIMZ_") || line.starts_with("RIMZ="))
        );
        let password = helper_env
            .lines()
            .find_map(|line| line.strip_prefix("OPENCODE_PASSWORD="))
            .unwrap();
        let auth = format!(
            "authorization: Basic {}",
            base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}"))
        );
        for request in &requests {
            assert!(request.lines().any(|line| line.eq_ignore_ascii_case(&auth)));
        }
        let final_env = std::fs::read_to_string(env.home_root.join("final-env")).unwrap();
        for expected in [
            "RIMZ_LAUNCH_REMINDERS=keep for the TUI",
            "RIMZ_AGENT_MODEL=not-the-requested-model",
            "OPENCODE_CONFIG_CONTENT={\"theme\":\"dark\"}",
        ] {
            assert!(
                final_env.lines().any(|line| line == expected),
                "missing {expected}"
            );
        }
        assert!(
            !final_env
                .lines()
                .any(|line| line.starts_with("OPENCODE_PASSWORD="))
        );
    }
}

#[test]
fn hidden_step_refuses_unknown_models_and_http_errors_without_exec() {
    let gone = r#"{"_tag":"SessionNotFoundError","message":"Session not found: ses_gone"}"#;
    for (model, session, routes, reason) in [
        (
            "deepseek-4-1-flash",
            None,
            vec![("GET /api/model?", 200, CATALOG.into())],
            "unknown model; did you mean opencode-go/deepseek-v4.1-flash?",
        ),
        (
            "deepseek-4-1-flash",
            None,
            vec![("GET /api/model?", 503, "{}".into())],
            "model list returned HTTP 503 Service Unavailable; model selection is verified on OpenCode 2.1.4; ",
        ),
        (
            "deepseek-v4.1-flash",
            Some("ses_gone"),
            vec![
                ("GET /api/model?", 200, CATALOG.into()),
                ("POST /api/session/ses_gone/model", 404, gone.into()),
            ],
            "session model returned HTTP 404 Not Found: Session not found: ses_gone",
        ),
    ] {
        let env = Env::new();
        let requests = routes.len();
        let (origin, server) = crate::oauth_usage::serve_http_routes(routes, requests);
        let output = step_command(&env, &origin, model, session)
            .bounded_output()
            .unwrap();
        assert_eq!(output.status.code(), Some(78));
        let stderr = String::from_utf8(output.stderr).unwrap();
        let line = format!("rimz: OpenCode model check refused {model:?}: {reason}");
        if reason.ends_with("; ") {
            assert!(stderr.starts_with(&line), "{stderr}");
            assert!(stderr.contains("opencode upgrade --major"), "{stderr}");
        } else {
            // A name refusal or OpenCode's own message names the cause, not the version.
            assert_eq!(stderr.trim(), line);
        }
        assert_eq!(stderr.lines().count(), 1, "{stderr}");
        assert_eq!(server.join().unwrap().len(), requests);
        assert!(!env.home_root.join("final-argv").exists());
        assert_eq!(
            std::fs::read_to_string(env.home_root.join("serve-pids"))
                .unwrap()
                .lines()
                .count(),
            1
        );
    }
}

#[test]
fn killing_the_step_closes_the_helpers_stdin_lease() {
    let env = Env::new();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let (requested, request_seen) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let server = std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut line = String::new();
        std::io::BufReader::new(&stream)
            .read_line(&mut line)
            .unwrap();
        requested.send(line).unwrap();
        let _ = released.recv_timeout(Duration::from_secs(15));
    });
    let mut command = step_command(&env, &origin, "deepseek-v4.1-flash", None);
    let mut child = command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let request = request_seen.recv_timeout(Duration::from_secs(15));
    assert!(
        request
            .as_ref()
            .is_ok_and(|line| line.starts_with("GET /api/model?")),
        "the step must issue its model GET: {request:?}"
    );
    let pid: u32 = std::fs::read_to_string(env.home_root.join("serve-pids"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(
        rimz::proc::process_is_live(pid, None),
        "the helper must hold its stdin lease during the request"
    );
    child.kill().unwrap();
    child.wait().unwrap();
    release.send(()).unwrap();
    server.join().unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    while rimz::proc::process_is_live(pid, None) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!rimz::proc::process_is_live(pid, None));
    assert_eq!(
        std::fs::read_to_string(env.home_root.join("closed")).unwrap(),
        "closed\n"
    );
    assert!(!env.home_root.join("final-argv").exists());
}
