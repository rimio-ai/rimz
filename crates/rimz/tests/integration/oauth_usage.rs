use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;
use std::time::Duration;

use serde_json::Value;

use crate::common::{CommandTimeoutExt, Env};

#[test]
fn claude_refresh_usage_populates_windows_and_extra_credits_from_oauth_endpoint() {
    let env = Env::new();
    let (origin, server) = serve_once(
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
        .args([
            "claude",
            "refresh-usage",
            "--workspace-id",
            env.workspace_id.as_str(),
            "--merge-windows",
            "--agent-version",
            "2.1.173",
        ])
        .env(
            "RIMZ_CLAUDE_OAUTH_USAGE_URL",
            format!("{origin}/api/oauth/usage"),
        )
        .bounded_output()
        .expect("rimz claude refresh");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = server.join().expect("server request");
    assert!(request.starts_with("GET /api/oauth/usage "));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer claude-token")
    );
    assert!(
        request
            .to_ascii_lowercase()
            .contains("user-agent: claude-code/2.1.173")
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
    let limits = read_json(runtime.shared_rate_limits_path());
    assert_eq!(
        limits["windows"]["claude"]["windows"][0]["used_percentage"],
        13
    );
    assert_eq!(
        limits["windows"]["claude"]["windows"][1]["duration_mins"],
        10080
    );
}

#[test]
fn codex_refresh_rate_limits_falls_back_to_oauth_usage_when_app_server_is_unreachable() {
    let env = Env::new();
    let (origin, server) = serve_once(
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
        .args([
            "codex",
            "refresh-rate-limits",
            "--workspace-id",
            env.workspace_id.as_str(),
        ])
        .env("RIMZ_CODEX_BIN", env.home_root.join("missing-codex"))
        .env("RIMZ_CODEX_APP_SERVER_SOCK", "")
        .bounded_output()
        .expect("rimz codex refresh");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let request = server.join().expect("server request");
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
    let limits = read_json(runtime.shared_rate_limits_path());
    assert_eq!(
        limits["windows"]["codex"]["windows"][0]["used_percentage"],
        42
    );
    assert_eq!(
        limits["windows"]["codex"]["windows"][0]["duration_mins"],
        300
    );
    assert_eq!(
        limits["windows"]["codex"]["windows"][1]["duration_mins"],
        10080
    );
}

fn serve_once(body: &'static str) -> (String, thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind http stub");
    let addr = listener.local_addr().expect("local addr");
    let handle = thread::spawn(move || {
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
        let response = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
        String::from_utf8_lossy(&request).into_owned()
    });
    (format!("http://{addr}"), handle)
}

fn read_json(path: std::path::PathBuf) -> Value {
    serde_json::from_slice(&std::fs::read(path).expect("read json")).expect("parse json")
}
