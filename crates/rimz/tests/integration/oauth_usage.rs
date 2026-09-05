use std::io::{Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::common::{CommandTimeoutExt, Env, path_with_front};

fn refresh_usage_argv(env: &Env, kind: &str, claim_id: &str) -> Vec<String> {
    let request = rimz::sidebar::refresh::usage::AccountUsageRefreshRequest {
        workspace_id: env.workspace_id.clone(),
        kind: rimz::ids::AgentKind::new_unchecked(kind),
        claim_id: claim_id.parse().expect("valid usage claim id"),
    };
    rimz::child_process::agent_helper_argv("refresh-usage", &request)
}

#[test]
fn claude_old_workspace_session_cannot_repaint_switched_account_limits() {
    let env = Env::new();
    let old_project = env.home_root.join("old-project");
    std::fs::create_dir_all(&old_project).expect("mkdir old project");
    let old_workspace_id = rimz::WorkspaceId::from_project_root(&old_project);
    let claude_home = env.home_root.join(".claude");
    std::fs::create_dir_all(&claude_home).expect("mkdir claude home");
    write_claude_credentials(&claude_home, "old");

    let old_start = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "old-account-session",
        "source": "startup"
    })
    .to_string();
    let mut old_hook = env.hook_command("claude");
    old_hook.current_dir(&old_project);
    let output = env
        .spawn_payload(old_hook, &old_start)
        .wait_with_output()
        .expect("wait old-account SessionStart");
    assert!(
        output.status.success(),
        "old-account hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut old_statusline = env.statusline_feed_command("claude");
    old_statusline.current_dir(&old_project);
    let output = env
        .spawn_payload(
            old_statusline,
            &claude_statusline("old-account-session", 88, 19),
        )
        .wait_with_output()
        .expect("wait old-account statusline");
    assert!(
        output.status.success(),
        "old-account statusline stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    write_claude_credentials(&claude_home, "new");
    let new_start = serde_json::json!({
        "hook_event_name": "SessionStart",
        "session_id": "new-account-session",
        "source": "startup"
    })
    .to_string();
    let output = env.run_hook("claude", &new_start);
    assert!(
        output.status.success(),
        "new-account hook stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = env.run_statusline_feed("claude", &claude_statusline("new-account-session", 0, 0));
    assert!(
        output.status.success(),
        "new-account statusline stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let (origin, server) = serve_after_failures(
        0,
        r#"{
            "five_hour": { "utilization": 0, "resets_at": "2026-09-21T14:13:20Z" },
            "seven_day": { "utilization": 0, "resets_at": "2026-09-27T09:06:40Z" }
        }"#,
    );
    let bin_dir = env.home_root.join("bin");
    write_fake_claude(&bin_dir);
    let panes = env.write_pane_fixture(&[]);
    run_sidebar_snapshot(
        &env,
        &env.project_root,
        env.workspace_id.as_str(),
        &bin_dir,
        &panes,
        Some(&origin),
    );

    let runtime = env.runtime_paths();
    let deadline = Instant::now() + Duration::from_secs(10);
    let switched_key = loop {
        let limits = std::fs::read(runtime.shared_rate_limits_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let entry = limits.as_ref().map(|limits| &limits["entries"]["claude"]);
        let settled = entry.is_some_and(|entry| {
            entry["account_key"].as_str().is_some()
                && entry["bound_limits"]["windows"]
                    .as_array()
                    .is_some_and(|windows| {
                        windows.len() == 2
                            && windows.iter().all(|window| window["used_percentage"] == 0)
                    })
        });
        if settled {
            break entry
                .and_then(|entry| entry["account_key"].as_str())
                .expect("settled account key")
                .to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "switched account usage did not settle: limits={limits:?}"
        );
        thread::sleep(Duration::from_millis(20));
    };

    run_sidebar_snapshot(
        &env,
        &old_project,
        old_workspace_id.as_str(),
        &bin_dir,
        &panes,
        None,
    );
    let limits = read_json(runtime.shared_rate_limits_path());
    let entry = &limits["entries"]["claude"];
    assert_eq!(entry["account_key"], switched_key);
    assert!(
        entry["limits"]["windows"]
            .as_array()
            .is_some_and(|windows| windows.len() == 2
                && windows.iter().all(|window| window["used_percentage"] == 0)),
        "old workspace repainted switched-account limits: {entry}"
    );

    let output = env
        .rimz()
        .args(["providers", "claude", "--json"])
        .bounded_output()
        .expect("rimz providers claude");
    assert!(
        output.status.success(),
        "providers stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("providers json");
    assert!(
        report[0]["windows"].as_array().is_some_and(|windows| {
            windows.len() == 2 && windows.iter().all(|window| window["used_percentage"] == 0)
        }),
        "providers reported old-account limits: {report}"
    );
    assert_eq!(server.join().expect("server request").len(), 1);
}

#[test]
fn one_cold_snapshot_discovers_claude_and_publishes_first_usage_windows() {
    let env = Env::new();
    let (origin, server) = serve_after_failures(
        0,
        r#"{
            "five_hour": {
                "utilization": 12.5,
                "resets_at": "2026-09-21T14:13:20Z"
            },
            "seven_day": {
                "utilization": 7,
                "resets_at": "2026-09-27T09:06:40Z"
            }
        }"#,
    );
    let claude_home = env.home_root.join(".claude");
    std::fs::create_dir_all(&claude_home).expect("mkdir claude home");
    std::fs::write(
        claude_home.join(".credentials.json"),
        r#"{
            "claudeAiOauth": {
                "accessToken": "claude-token",
                "expiresAt": 4102444800000,
                "scopes": ["user:profile"]
            }
        }"#,
    )
    .expect("write claude credentials");
    let bin_dir = env.home_root.join("bin");
    std::fs::create_dir_all(&bin_dir).expect("mkdir fake bin");
    let claude = bin_dir.join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\n\
         if [ \"${1:-}\" = \"auth\" ] && [ \"${2:-}\" = \"status\" ]; then\n\
           printf '%s\\n' '{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"subscriptionType\":\"max\"}'\n\
           exit 0\n\
         fi\n\
         if [ \"${1:-}\" = \"--version\" ]; then\n\
           printf '%s\\n' '2.1.173 (Claude Code)'\n\
           exit 0\n\
         fi\n\
         exit 1\n",
    )
    .expect("write fake claude");
    let mut permissions = std::fs::metadata(&claude).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&claude, permissions).expect("chmod fake claude");
    let panes = env.write_pane_fixture(&[]);

    let output = env
        .rimz()
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--mux",
            "tmux",
            "--session-name",
            "rimz-test",
            "--json",
        ])
        .env("RIMZ_TEST_PANE_LIST", panes)
        .env("PATH", path_with_front(&bin_dir))
        .env(
            "RIMZ_CLAUDE_OAUTH_USAGE_URL",
            format!("{origin}/api/oauth/usage"),
        )
        .bounded_output()
        .expect("one cold sidebar snapshot");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let runtime = env.runtime_paths();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let credits = std::fs::read(runtime.shared_credits_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let limits = std::fs::read(runtime.shared_rate_limits_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let settled = credits.as_ref().is_some_and(|credits| {
            credits["entries"]["claude"]["oauth_read_at_ms"]
                .as_u64()
                .is_some_and(|stamp| stamp > 0)
                && credits["entries"]["claude"]["direct_query_claim"].is_null()
        });
        let window_count = limits
            .as_ref()
            .and_then(|limits| limits["entries"]["claude"]["limits"]["windows"].as_array())
            .map_or(0, Vec::len);
        if settled && window_count == 2 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "first snapshot did not settle Claude usage: credits={credits:?}, limits={limits:?}"
        );
        thread::sleep(Duration::from_millis(20));
    }

    let requests = server.join().expect("server request");
    assert_eq!(requests.len(), 1);
}

#[test]
fn claude_refresh_usage_populates_windows_and_extra_credits_from_oauth_endpoint() {
    let env = Env::new();
    let claim_id = env.seed_usage_claim("claude");
    let (origin, server) = serve_after_failures(
        0,
        r#"{
            "five_hour": {
                "utilization": 12.5,
                "resets_at": "2026-09-21T14:13:20Z"
            },
            "seven_day": {
                "utilization": 7,
                "resets_at": "2026-09-27T09:06:40Z"
            },
            "extra_usage": {
                "is_enabled": true,
                "used_credits": 725,
                "monthly_limit": 5000
            }
        }"#,
    );
    let claude_home = env.home_root.join(".claude");
    std::fs::create_dir_all(&claude_home).expect("mkdir claude home");
    std::fs::write(
        claude_home.join(".credentials.json"),
        r#"{
            "claudeAiOauth": {
                "accessToken": "claude-token",
                "expiresAt": 4102444800000,
                "scopes": ["user:profile"]
            }
        }"#,
    )
    .expect("write claude credentials");

    let output = env
        .rimz()
        .args(refresh_usage_argv(&env, "claude", &claim_id))
        .env(
            "RIMZ_CLAUDE_OAUTH_USAGE_URL",
            format!("{origin}/api/oauth/usage"),
        )
        .bounded_output()
        .expect("rimz agents refresh-usage claude");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = server.join().expect("server request");
    let request = &requests[0];
    assert!(request.starts_with("GET /api/oauth/usage "));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer claude-token")
    );
    // The Claude version rides the user-agent; it now resolves from the local
    // binary rather than a passed flag, so assert the product prefix only.
    assert!(
        request
            .to_ascii_lowercase()
            .contains("user-agent: claude-code/")
    );

    let runtime = env.runtime_paths();
    let credits = read_json(runtime.shared_credits_path());
    assert_eq!(
        credits["entries"]["claude"]["extra_credits"]["known"]["used_usd"],
        7.25
    );
    assert_eq!(
        credits["entries"]["claude"]["extra_credits"]["known"]["limit_usd"],
        50.0
    );
    assert_eq!(
        credits["entries"]["claude"]["account_key"]
            .as_str()
            .map(str::len),
        Some(64)
    );
    assert_ne!(credits["entries"]["claude"]["account_key"], "claude-token");
    let limits = read_json(runtime.shared_rate_limits_path());
    assert_eq!(
        limits["entries"]["claude"]["limits"]["windows"][0]["used_percentage"],
        13
    );
    assert_eq!(
        limits["entries"]["claude"]["limits"]["windows"][1]["duration_mins"],
        10080
    );

    let mut session_limits = limits;
    for window in session_limits["entries"]["claude"]["limits"]["windows"]
        .as_array_mut()
        .expect("cached windows")
    {
        window["used_percentage"] = serde_json::json!(19);
        window["resets_at"] = serde_json::json!("2100-01-01T00:00:00Z");
        window["observed_at"] =
            serde_json::json!(jiff::Timestamp::now() - jiff::SignedDuration::from_secs(60));
        window["source"] = serde_json::json!("best_effort");
    }
    std::fs::write(
        runtime.shared_rate_limits_path(),
        serde_json::to_vec(&session_limits).expect("serialize session limits"),
    )
    .expect("publish conflicting session limits");
    let claim_id = env.seed_usage_claim("claude");
    let (origin, server) = serve_after_failures(
        0,
        r#"{
            "five_hour": { "utilization": 13, "resets_at": "2100-01-01T00:00:00Z" },
            "seven_day": { "utilization": 4, "resets_at": "2100-01-01T00:00:00Z" }
        }"#,
    );
    let output = env
        .rimz()
        .args(refresh_usage_argv(&env, "claude", &claim_id))
        .env(
            "RIMZ_CLAUDE_OAUTH_USAGE_URL",
            format!("{origin}/api/oauth/usage"),
        )
        .bounded_output()
        .expect("refresh conflicting usage");
    assert!(
        output.status.success(),
        "refresh stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(server.join().expect("correction request").len(), 1);

    let bin_dir = env.home_root.join("bin");
    write_fake_claude(&bin_dir);
    let output = env
        .rimz()
        .args(["providers", "claude", "--json"])
        .env("PATH", path_with_front(&bin_dir))
        .bounded_output()
        .expect("read corrected provider usage");
    assert!(
        output.status.success(),
        "providers stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("providers json");
    let weekly = report[0]["windows"]
        .as_array()
        .expect("provider windows")
        .iter()
        .find(|window| window["duration_mins"] == 10_080)
        .expect("weekly window");
    assert_eq!(
        weekly["used_percentage"], 4,
        "providers retained session usage: {report}"
    );
}

#[test]
fn claude_refresh_usage_refuses_an_untrusted_override_without_publishing_usage() {
    let env = Env::new();
    let claim_id = env.seed_usage_claim("claude");
    let claude_home = env.home_root.join(".claude");
    std::fs::create_dir_all(&claude_home).expect("mkdir claude home");
    std::fs::write(
        claude_home.join(".credentials.json"),
        r#"{
            "claudeAiOauth": {
                "accessToken": "claude-token",
                "expiresAt": 4102444800000,
                "scopes": ["user:profile"]
            }
        }"#,
    )
    .expect("write claude credentials");

    let output = env
        .rimz()
        .args(refresh_usage_argv(&env, "claude", &claim_id))
        .env(
            "RIMZ_CLAUDE_OAUTH_USAGE_URL",
            "https://rimz-advisory.invalid/api/oauth/usage",
        )
        .bounded_output()
        .expect("rimz agents refresh-usage claude");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let runtime = env.runtime_paths();
    let credits = read_json(runtime.shared_credits_path());
    assert!(credits["entries"]["claude"]["oauth_read_at_ms"].as_u64() > Some(1));
    assert_eq!(credits["entries"]["claude"]["auth_settled"], true);
    assert!(credits["entries"]["claude"]["direct_query_claim"].is_null());
    assert!(
        std::fs::read(runtime.shared_rate_limits_path()).is_err(),
        "an untrusted endpoint must not publish usage windows"
    );
}

#[test]
fn claude_refresh_usage_retries_transient_http_failures() {
    let env = Env::new();
    let claim_id = env.seed_usage_claim("claude");
    let (origin, server) = serve_after_failures(
        2,
        r#"{
            "five_hour": {
                "utilization": 12.5,
                "resets_at": "2026-09-21T14:13:20Z"
            }
        }"#,
    );
    let claude_home = env.home_root.join(".claude");
    std::fs::create_dir_all(&claude_home).expect("mkdir claude home");
    std::fs::write(
        claude_home.join(".credentials.json"),
        r#"{
            "claudeAiOauth": {
                "accessToken": "claude-token",
                "expiresAt": 4102444800000,
                "scopes": ["user:profile"]
            }
        }"#,
    )
    .expect("write claude credentials");

    let output = env
        .rimz()
        .args(refresh_usage_argv(&env, "claude", &claim_id))
        .env(
            "RIMZ_CLAUDE_OAUTH_USAGE_URL",
            format!("{origin}/api/oauth/usage"),
        )
        .bounded_output()
        .expect("rimz agents refresh-usage claude");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = server.join().expect("server requests");
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| request.starts_with("GET /api/oauth/usage "))
    );

    let limits = read_json(env.runtime_paths().shared_rate_limits_path());
    assert!(
        limits["entries"]["claude"]["limits"]["windows"]
            .as_array()
            .is_some_and(|windows| !windows.is_empty())
    );
}

#[test]
fn agents_refresh_usage_codex_falls_back_to_oauth_usage_when_app_server_is_unreachable() {
    let env = Env::new();
    let claim_id = env.seed_usage_claim("codex");
    let (origin, server) = serve_after_failures(
        0,
        r#"{
            "plan_type": "pro",
            "rate_limit": {
                "primary_window": {
                    "used_percent": 42,
                    "reset_at": 1780092691,
                    "limit_window_seconds": 18000
                },
                "secondary_window": {
                    "used_percent": 7,
                    "reset_at": 1780186207,
                    "limit_window_seconds": 604800
                }
            },
            "credits": { "balance": 18.5 }
        }"#,
    );
    let codex_home = env.home_root.join(".codex");
    std::fs::create_dir_all(&codex_home).expect("mkdir codex home");
    std::fs::write(
        codex_home.join("auth.json"),
        r#"{
            "OPENAI_API_KEY": null,
            "tokens": {
                "access_token": "codex-token",
                "account_id": "acc_123"
            }
        }"#,
    )
    .expect("write codex auth");
    std::fs::write(
        codex_home.join("config.toml"),
        format!("chatgpt_base_url = \"{origin}/backend-api\"\n"),
    )
    .expect("write codex config");

    let output = env
        .rimz()
        .args(refresh_usage_argv(&env, "codex", &claim_id))
        .env("RIMZ_CODEX_BIN", env.home_root.join("missing-codex"))
        .env("RIMZ_CODEX_APP_SERVER_SOCK", "")
        .bounded_output()
        .expect("rimz agents refresh-usage codex");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = server.join().expect("server request");
    let request = &requests[0];
    assert!(request.starts_with("GET /backend-api/wham/usage "));
    let request_lower = request.to_ascii_lowercase();
    assert!(request_lower.contains("authorization: bearer codex-token"));
    assert!(request_lower.contains("chatgpt-account-id: acc_123"));

    let runtime = env.runtime_paths();
    let credits = read_json(runtime.shared_credits_path());
    assert_eq!(
        credits["entries"]["codex"]["extra_credits"]["known"]["remaining_usd"],
        18.5
    );
    assert_eq!(credits["entries"]["codex"]["account_key"], "acc_123");
    let limits = read_json(runtime.shared_rate_limits_path());
    assert_eq!(
        limits["entries"]["codex"]["limits"]["windows"][0]["used_percentage"],
        42
    );
    assert_eq!(
        limits["entries"]["codex"]["limits"]["windows"][0]["duration_mins"],
        300
    );
    assert_eq!(
        limits["entries"]["codex"]["limits"]["windows"][1]["duration_mins"],
        10080
    );
}

fn serve_after_failures(
    failures: usize,
    body: &'static str,
) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind http stub");
    let addr = listener.local_addr().expect("local addr");
    let handle = thread::spawn(move || {
        let mut requests = Vec::with_capacity(failures + 1);
        for response_index in 0..=failures {
            let (mut stream, _) = listener.accept().expect("accept request");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set read timeout");
            let mut request = Vec::new();
            let mut buf = [0_u8; 1024];
            loop {
                let read = stream.read(&mut buf).expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            requests.push(String::from_utf8_lossy(&request).into_owned());

            let (status, response_body) = if response_index < failures {
                ("500 Internal Server Error", "")
            } else {
                ("200 OK", body)
            };
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{response_body}",
                response_body.len(),
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        }
        requests
    });
    (format!("http://{addr}"), handle)
}

fn read_json(path: std::path::PathBuf) -> Value {
    serde_json::from_slice(&std::fs::read(path).expect("read json")).expect("parse json")
}

fn write_claude_credentials(claude_home: &std::path::Path, refresh_token: &str) {
    std::fs::write(
        claude_home.join(".credentials.json"),
        serde_json::json!({
            "claudeAiOauth": {
                "accessToken": format!("access-{refresh_token}"),
                "refreshToken": refresh_token,
                "expiresAt": 4102444800000_u64,
                "scopes": ["user:profile"]
            }
        })
        .to_string(),
    )
    .expect("write claude credentials");
}

fn write_fake_claude(bin_dir: &std::path::Path) {
    std::fs::create_dir_all(bin_dir).expect("mkdir fake bin");
    let claude = bin_dir.join("claude");
    std::fs::write(
        &claude,
        "#!/bin/sh\n\
         if [ \"${1:-}\" = \"auth\" ] && [ \"${2:-}\" = \"status\" ]; then\n\
           printf '%s\\n' '{\"loggedIn\":true,\"authMethod\":\"claude.ai\",\"subscriptionType\":\"max\"}'\n\
           exit 0\n\
         fi\n\
         if [ \"${1:-}\" = \"--version\" ]; then\n\
           printf '%s\\n' '2.1.173 (Claude Code)'\n\
           exit 0\n\
         fi\n\
         exit 1\n",
    )
    .expect("write fake claude");
    let mut permissions = std::fs::metadata(&claude).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&claude, permissions).expect("chmod fake claude");
}

fn claude_statusline(session_id: &str, five_hour: u8, seven_day: u8) -> String {
    serde_json::json!({
        "session_id": session_id,
        "model": { "id": "claude-opus-4-6", "display_name": "Opus" },
        "version": "2.1.173",
        "rate_limits": {
            "five_hour": { "used_percentage": five_hour, "resets_at": 4102444800_u64 },
            "seven_day": { "used_percentage": seven_day, "resets_at": 4102444800_u64 }
        }
    })
    .to_string()
}

fn run_sidebar_snapshot(
    env: &Env,
    project_root: &std::path::Path,
    workspace_id: &str,
    bin_dir: &std::path::Path,
    panes: &std::path::Path,
    oauth_origin: Option<&str>,
) {
    let mut command = env.rimz();
    command
        .args([
            "sidebar",
            "snapshot",
            "--workspace-id",
            workspace_id,
            "--mux",
            "tmux",
            "--session-name",
            "rimz-test",
            "--json",
        ])
        .current_dir(project_root)
        .env("RIMZ_TEST_PANE_LIST", panes)
        .env("PATH", path_with_front(bin_dir));
    if let Some(origin) = oauth_origin {
        command.env(
            "RIMZ_CLAUDE_OAUTH_USAGE_URL",
            format!("{origin}/api/oauth/usage"),
        );
    }
    let output = command.bounded_output().expect("rimz sidebar snapshot");
    assert!(
        output.status.success(),
        "sidebar snapshot stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
