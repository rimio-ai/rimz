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
        login: rimz::ids::LoginKey::default_for(rimz::ids::AgentKind::new_unchecked(kind)),
        claim_id: claim_id.parse().expect("valid usage claim id"),
    };
    rimz::child_process::agent_helper_argv("refresh-usage", &request)
}

#[test]
fn claude_organization_rejection_records_lapsed_entitlement_without_body_text() {
    let env = Env::new();
    let claim_id = env.seed_usage_claim("claude");
    let home = env.home_root.join(".claude");
    std::fs::create_dir_all(&home).unwrap();
    write_claude_credentials(&home, "work");
    let (origin, server) = serve_http_routes(vec![("GET /api/oauth/usage", 403,
        r#"{"type":"error","error":{"type":"permission_error","message":"sentinel-private-body","details":{"error_visibility":"user_facing","error_code":"oauth_not_allowed_for_organization"}}}"#.to_owned())], 1);
    let output = env
        .rimz()
        .args(refresh_usage_argv(&env, "claude", &claim_id))
        .env(
            "RIMZ_CLAUDE_OAUTH_USAGE_URL",
            format!("{origin}/api/oauth/usage"),
        )
        .bounded_output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stderr).contains("sentinel-private-body"));
    assert_eq!(server.join().unwrap().len(), 1);
    let cache = read_json(env.runtime_paths().shared_credits_path());
    let entry = &cache["logins"]["claude@default"];
    assert!(
        entry["entitlement"]["lapsed"]["since_ms"]
            .as_u64()
            .is_some_and(|since| since > 0),
        "{entry}"
    );
    assert_eq!(entry["auth_settled"], true);
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
    env.drain_hooks_for(&old_project);
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
            "five_hour": { "utilization": 0, "resets_at": "2100-01-01T00:00:00Z" },
            "seven_day": { "utilization": 0, "resets_at": "2100-01-01T00:00:00Z" }
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
        let entry = limits
            .as_ref()
            .map(|limits| &limits["entries"]["claude@default"]);
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
    let entry = &limits["entries"]["claude@default"];
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
                "resets_at": "2100-01-01T00:00:00Z"
            },
            "seven_day": {
                "utilization": 7,
                "resets_at": "2100-01-01T00:00:00Z"
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
            credits["logins"]["claude@default"]["oauth_read_at_ms"]
                .as_u64()
                .is_some_and(|stamp| stamp > 0)
                && credits["logins"]["claude@default"]["direct_query_claim"].is_null()
        });
        let window_count = limits
            .as_ref()
            .and_then(|limits| limits["entries"]["claude@default"]["limits"]["windows"].as_array())
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
            "cedar_ember": {"eligible": true, "grants": [{"resets_left": 1, "ends_at": "2026-10-22T16:00:00Z"}]},
            "five_hour": {
                "utilization": 12.5,
                "resets_at": "2100-01-01T00:00:00Z"
            },
            "seven_day": {
                "utilization": 7,
                "resets_at": "2100-01-01T00:00:00Z"
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
    assert!(request.starts_with("GET /api/oauth/usage?cedar_ember=1 "));
    assert!(
        request
            .to_ascii_lowercase()
            .contains("authorization: bearer claude-token")
    );
    // The version resolves from the local binary or the adapter's fallback.
    assert!(
        request
            .to_ascii_lowercase()
            .lines()
            .any(|line| line.starts_with("user-agent: claude-cli/")
                && line.ends_with(" (external, cli)"))
    );

    let runtime = env.runtime_paths();
    let credits = read_json(runtime.shared_credits_path());
    assert_eq!(
        credits["logins"]["claude@default"]["reset_credits"]["count"],
        1
    );
    assert_eq!(
        credits["logins"]["claude@default"]["extra_credits"]["known"]["used_usd"],
        7.25
    );
    assert_eq!(
        credits["logins"]["claude@default"]["extra_credits"]["known"]["limit_usd"],
        50.0
    );
    assert_eq!(
        credits["logins"]["claude@default"]["account_key"]
            .as_str()
            .map(str::len),
        Some(64)
    );
    assert_ne!(
        credits["logins"]["claude@default"]["account_key"],
        "claude-token"
    );
    let limits = read_json(runtime.shared_rate_limits_path());
    assert_eq!(
        limits["entries"]["claude@default"]["limits"]["windows"][0]["used_percentage"],
        13
    );
    assert_eq!(
        limits["entries"]["claude@default"]["limits"]["windows"][1]["duration_mins"],
        10080
    );

    let mut session_limits = limits;
    for window in session_limits["entries"]["claude@default"]["limits"]["windows"]
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
    assert!(credits["logins"]["claude@default"]["oauth_read_at_ms"].as_u64() > Some(1));
    assert_eq!(credits["logins"]["claude@default"]["auth_settled"], true);
    assert!(credits["logins"]["claude@default"]["direct_query_claim"].is_null());
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
                "resets_at": "2100-01-01T00:00:00Z"
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
            .all(|request| request.starts_with("GET /api/oauth/usage?cedar_ember=1 "))
    );

    let limits = read_json(env.runtime_paths().shared_rate_limits_path());
    assert!(
        limits["entries"]["claude@default"]["limits"]["windows"]
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
        credits["logins"]["codex@default"]["extra_credits"]["known"]["remaining_usd"],
        0.74
    );
    assert_eq!(credits["logins"]["codex@default"]["account_key"], "acc_123");
    let limits = read_json(runtime.shared_rate_limits_path());
    assert_eq!(
        limits["entries"]["codex@default"]["limits"]["windows"][0]["used_percentage"],
        42
    );
    assert_eq!(
        limits["entries"]["codex@default"]["limits"]["windows"][0]["duration_mins"],
        300
    );
    assert_eq!(
        limits["entries"]["codex@default"]["limits"]["windows"][1]["duration_mins"],
        10080
    );
}

#[test]
fn auto_redeem_rescues_an_idle_codex_account_and_records_its_login() {
    let env = Env::new();
    let now = jiff::Timestamp::now();
    let reset_at = |secs| (now + jiff::SignedDuration::from_secs(secs)).as_second();
    let usage = serde_json::json!({
        "plan_type": "pro",
        "rate_limit": {
            "primary_window": {
                "used_percent": 42,
                "reset_at": reset_at(3600),
                "limit_window_seconds": 18000
            },
            "secondary_window": {
                "used_percent": 7,
                "reset_at": reset_at(3 * 86400),
                "limit_window_seconds": 604800
            }
        }
    });
    let reset_credits = serde_json::json!({
        "available_count": 1,
        "credits": [{
            "id": "credit-1",
            "status": "available",
            "expires_at": now + jiff::SignedDuration::from_secs(10 * 60)
        }]
    });
    let (origin, server) = serve_routes(
        vec![
            ("GET /backend-api/wham/usage ", usage.to_string()),
            (
                "GET /backend-api/wham/rate-limit-reset-credits ",
                reset_credits.to_string(),
            ),
            (
                "POST /backend-api/wham/rate-limit-reset-credits/consume ",
                r#"{"code":"reset","windows_reset":2}"#.to_owned(),
            ),
        ],
        5,
    );
    let spare_home = env.home_root.join("spare");
    std::fs::create_dir_all(&spare_home).expect("mkdir spare codex home");
    std::fs::write(
        spare_home.join("auth.json"),
        r#"{"tokens": {"access_token": "spare-token", "account_id": "acc_spare"}}"#,
    )
    .expect("write codex auth");
    std::fs::write(
        spare_home.join("config.toml"),
        format!("chatgpt_base_url = \"{origin}/backend-api\"\n"),
    )
    .expect("write codex config");
    std::fs::create_dir_all(env.rimz_home()).expect("mkdir rimz home");
    std::fs::write(
        env.rimz_home().join("config.toml"),
        format!(
            "[accounts.codex.spare]\nhome = {:?}\n",
            spare_home.to_str().expect("utf-8 home")
        ),
    )
    .expect("write machine config");

    let request = rimz::harness::auto_redeem::AutoRedeemRequest {
        workspace_id: env.workspace_id.clone(),
        login: "codex@spare".parse().expect("login key"),
        reason: rimz::harness::auto_redeem::RedeemReason::ExpiryRescue,
        request_id: uuid::Uuid::now_v7(),
        limit_paused: false,
    };
    let output = env
        .rimz()
        .args(rimz::child_process::agent_helper_argv(
            "auto-redeem",
            &request,
        ))
        .bounded_output()
        .expect("rimz agents auto-redeem");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let records = rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None);
    let [record] = records.as_slice() else {
        panic!("expected one assist record: {records:?}");
    };
    let rimz::harness::assist_log::Assist::AutoRedeem {
        kind,
        login,
        reason,
        request_id,
        outcome,
        error,
        ..
    } = &record.assist
    else {
        panic!("expected an auto-redeem record: {record:?}");
    };
    assert_eq!(kind, "codex");
    assert_eq!(login.as_ref().map(|login| login.as_str()), Some("spare"));
    assert_eq!(
        *reason,
        rimz::harness::auto_redeem::RedeemReason::ExpiryRescue
    );
    assert_eq!(request_id, &request.request_id.to_string());
    assert_eq!(outcome.as_deref(), Some("reset"));
    assert_eq!(*error, None);

    let requests = server.join().expect("server requests");
    let consume = requests
        .iter()
        .find(|request| request.starts_with("POST "))
        .expect("consume request");
    assert!(
        consume
            .to_ascii_lowercase()
            .contains("authorization: bearer spare-token")
    );
    let body: Value = serde_json::from_str(consume.split_once("\r\n\r\n").expect("request body").1)
        .expect("consume body json");
    assert_eq!(body["redeem_request_id"], request.request_id.to_string());
    assert_eq!(body["credit_id"], "credit-1");
}

fn manual_redeem_fixture(env: &Env, count: usize, code: &str) -> thread::JoinHandle<Vec<String>> {
    env.install_agent_hooks("codex");
    crate::common::trust_codex_hooks(env);
    let now = jiff::Timestamp::now();
    let usage = serde_json::json!({
        "plan_type": "pro",
        "rate_limit": {
            "primary_window": {"used_percent": 42, "reset_at": (now + Duration::from_secs(3600)).as_second(), "limit_window_seconds": 18000},
            "secondary_window": {"used_percent": 7, "reset_at": (now + Duration::from_secs(3 * 86400)).as_second(), "limit_window_seconds": 604800}
        }
    });
    let credits = serde_json::json!({
        "available_count": 1,
        "credits": [{"id": "credit-1", "status": "available", "expires_at": now + Duration::from_secs(10 * 60)}]
    });
    let (origin, server) = serve_routes(
        vec![
            ("GET /backend-api/wham/usage ", usage.to_string()),
            (
                "GET /backend-api/wham/rate-limit-reset-credits ",
                credits.to_string(),
            ),
            (
                "POST /backend-api/wham/rate-limit-reset-credits/consume ",
                serde_json::json!({"code": code, "windows_reset": if code == "reset" {2} else {0}})
                    .to_string(),
            ),
        ],
        count,
    );
    let home = env.home_root.join("spare");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("auth.json"),
        r#"{"tokens":{"access_token":"spare-token","account_id":"acc_spare"}}"#,
    )
    .unwrap();
    let native = env.agent_config_path("codex");
    let config = home.join("config.toml");
    let mut settings: toml::Table = std::fs::read_to_string(&native).unwrap().parse().unwrap();
    settings.insert(
        "chatgpt_base_url".to_owned(),
        format!("{origin}/backend-api").into(),
    );
    let state = settings["hooks"]["state"].as_table_mut().unwrap();
    let prefix = format!("{}:", native.display());
    let trusted = state
        .iter()
        .filter_map(|(key, value)| {
            Some((
                format!("{}:{}", config.display(), key.strip_prefix(&prefix)?),
                value.clone(),
            ))
        })
        .collect::<Vec<_>>();
    state.extend(trusted);
    std::fs::write(&config, toml::to_string(&settings).unwrap()).unwrap();
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        format!(
            "[accounts.codex.spare]\nhome = {:?}\n",
            home.to_str().unwrap()
        ),
    )
    .unwrap();
    server
}

#[test]
fn accounts_redeem_spends_one_credit_and_records_the_manual_login() {
    let env = Env::new();
    // Two preview GETs, consume, then two refresh GETs.
    let server = manual_redeem_fixture(&env, 5, "reset");
    let output = env
        .rimz()
        .args(["accounts", "redeem", "codex", "spare", "--yes"])
        .bounded_output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    for fact in [
        "codex@spare",
        "credits",
        "42% used",
        "7% used",
        "refills now",
        "forecast",
        "reset",
    ] {
        assert!(stdout.contains(fact), "missing {fact}: {stdout}");
    }
    let records = rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None);
    let [record] = records.as_slice() else {
        panic!("{records:?}")
    };
    let value = serde_json::to_value(record).unwrap();
    assert_eq!(value["assist"], "auto_redeem");
    assert_eq!(value["reason"], "manual");
    assert_eq!(value["login"], "spare");
    assert_eq!(value["outcome"], "reset");
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 5);
    let consume = &requests[2];
    assert!(consume.starts_with("POST /backend-api/wham/rate-limit-reset-credits/consume "));
    let body: Value = serde_json::from_str(consume.split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(body["credit_id"], "credit-1");
    assert_eq!(body["redeem_request_id"], value["request_id"]);
    assert_eq!(
        uuid::Uuid::parse_str(value["request_id"].as_str().unwrap())
            .unwrap()
            .get_version_num(),
        7
    );
    let stamp: Value = serde_json::from_slice(
        &std::fs::read(
            env.runtime_paths()
                .shared_credits_path()
                .with_file_name("auto_redeem.codex@spare.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(stamp["reason"], "manual");
    assert_eq!(stamp["outcome"], "reset");
}

#[test]
fn accounts_redeem_nothing_to_reset_exits_four_and_records_the_outcome() {
    let env = Env::new();
    // Two preview GETs and consume; no refresh for a non-reset.
    let server = manual_redeem_fixture(&env, 3, "nothing_to_reset");
    let output = env
        .rimz()
        .args(["accounts", "redeem", "codex", "spare", "--yes"])
        .bounded_output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(4),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let records = rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None);
    let [record] = records.as_slice() else {
        panic!("{records:?}")
    };
    assert_eq!(
        serde_json::to_value(record).unwrap()["outcome"],
        "nothing_to_reset"
    );
    assert_eq!(server.join().unwrap().len(), 3);
}

#[test]
fn accounts_redeem_dry_run_reads_only_and_leaves_no_stamp_or_record() {
    let env = Env::new();
    // Only the two preview GETs.
    let server = manual_redeem_fixture(&env, 2, "reset");
    let output = env
        .rimz()
        .args(["accounts", "redeem", "codex", "spare", "--dry-run"])
        .bounded_output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("codex@spare"));
    assert!(rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None).is_empty());
    assert!(
        !env.runtime_paths()
            .shared_credits_path()
            .with_file_name("auto_redeem.codex@spare.json")
            .exists()
    );
    assert!(
        server
            .join()
            .unwrap()
            .iter()
            .all(|request| request.starts_with("GET "))
    );
}

#[test]
fn accounts_redeem_non_terminal_refuses_before_provider_reads() {
    let env = Env::new();
    let output = env
        .rimz()
        .args(["accounts", "redeem", "codex"])
        .stdin(std::process::Stdio::null())
        .bounded_output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--yes"), "{stderr}");
    let output = env
        .rimz()
        .args(["accounts", "redeem", "claude"])
        .bounded_output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--yes"), "{stderr}");
}

/// Answer `count` requests, each with the body of the route its request line
/// starts with, and return every request with its body. One request past
/// `count` is served and returned too when it arrives within
/// `STRAY_REQUEST_WINDOW` of the last expected one, so a caller's count
/// assertion fails on a request the test did not expect.
fn serve_routes(
    routes: Vec<(&'static str, String)>,
    count: usize,
) -> (String, thread::JoinHandle<Vec<String>>) {
    serve_http_routes(
        routes
            .into_iter()
            .map(|(path, body)| (path, 200, body))
            .collect(),
        count,
    )
}

const STRAY_REQUEST_WINDOW: Duration = Duration::from_millis(500);

fn serve_http_routes(
    routes: Vec<(&'static str, u16, String)>,
    count: usize,
) -> (String, thread::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind http stub");
    let addr = listener.local_addr().expect("local addr");
    listener.set_nonblocking(true).unwrap();
    let handle = thread::spawn(move || {
        let mut requests = Vec::new();
        let accept_window = |served: usize| {
            if served < count {
                Duration::from_secs(15)
            } else {
                STRAY_REQUEST_WINDOW
            }
        };
        let mut deadline = Instant::now() + accept_window(0);
        while requests.len() <= count {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                    continue;
                }
                Err(error) => panic!("accept request: {error}"),
            };
            stream.set_nonblocking(false).expect("set blocking stream");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .expect("set read timeout");
            let mut request = Vec::new();
            let mut buf = [0_u8; 1024];
            loop {
                let text = String::from_utf8_lossy(&request);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let content_length = head
                        .lines()
                        .filter_map(|line| line.split_once(':'))
                        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map_or(0, |(_, value)| {
                            value.trim().parse::<usize>().expect("content-length")
                        });
                    if body.len() >= content_length {
                        break;
                    }
                }
                let read = stream.read(&mut buf).expect("read request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..read]);
            }
            let request = String::from_utf8_lossy(&request).into_owned();
            let matching: Vec<_> = routes
                .iter()
                .filter(|(route, _, _)| request.starts_with(route))
                .collect();
            let previous = matching.first().map_or(0, |(route, _, _)| {
                requests
                    .iter()
                    .filter(|request: &&String| request.starts_with(route))
                    .count()
            });
            let (status, body) = matching
                .get(previous.min(matching.len().saturating_sub(1)))
                .map_or((404, ""), |(_, status, body)| (*status, body.as_str()));
            requests.push(request);
            deadline = Instant::now() + accept_window(requests.len());
            if status == 0 {
                continue;
            }
            let response = format!(
                "HTTP/1.1 {status} Stub\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len(),
            );
            stream
                .write_all(response.as_bytes())
                .expect("write response");
        }
        requests
    });
    (format!("http://{addr}"), handle)
}

fn claude_redeem_usage() -> Value {
    serde_json::json!({
        "five_hour": {"utilization": 42, "resets_at": "2100-01-01T00:00:00Z"},
        "seven_day": {"utilization": 7, "resets_at": "2100-01-02T00:00:00Z"},
        "cedar_ember": {"eligible": true, "next_grant_id": "selected", "grants": [
            {"id": "other", "resets_left": 1, "usable_now": true, "paused": true},
            {"id": "selected", "resets_left": 1, "usable_now": true, "ends_at": "2100-02-01T00:00:00Z"}
        ]}
    })
}

#[derive(Default)]
struct ClaudeRedeemOptions {
    named_org: bool,
    omit_cleared: bool,
    fail_refresh: bool,
}

fn claude_redeem_fixture(
    env: &Env,
    usage: Value,
    profile_status: u16,
    claim_status: u16,
    result: &str,
    count: usize,
    options: ClaudeRedeemOptions,
) -> (String, thread::JoinHandle<Vec<String>>) {
    env.install_agent_hooks("claude");
    let native = env.home_root.join(".claude");
    let spare = env.home_root.join("spare");
    for (home, token) in [(&native, "default"), (&spare, "spare")] {
        std::fs::create_dir_all(home).unwrap();
        write_claude_credentials(home, token);
    }
    std::fs::copy(native.join("settings.json"), spare.join("settings.json")).unwrap();
    write_fake_claude(&env.home_root.join("bin"));
    std::fs::create_dir_all(env.rimz_home()).unwrap();
    std::fs::write(
        env.rimz_home().join("config.toml"),
        format!(
            "[resume]\nauto_redeem = true\n[accounts.claude.spare]\nhome = {:?}\n",
            spare.to_str().unwrap()
        ),
    )
    .unwrap();
    let org = if options.named_org {
        "spare-org"
    } else {
        "profile-org"
    };
    let claim_route = if options.named_org {
        "POST /api/organizations/spare-org/reset_rate_limits "
    } else {
        "POST /api/organizations/profile-org/reset_rate_limits "
    };
    let mut refreshed = usage.clone();
    if claim_status == 200 && result == "reset" {
        refreshed["cedar_ember"]["grants"][1]["resets_left"] = 0.into();
        refreshed["extra_usage"] = serde_json::json!({
            "is_enabled": true, "used_credits": 725, "monthly_limit": 5000
        });
    }
    let mut response = serde_json::json!({"result": result, "reason": "provider reason", "cleared": if result == "reset" {vec!["five_hour", "seven_day"]} else {Vec::new()}});
    if options.omit_cleared {
        response.as_object_mut().unwrap().remove("cleared");
    }
    let (origin, server) = serve_http_routes(
        vec![
            (
                "GET /api/oauth/usage?cedar_ember=1&skip_spend=1 ",
                200,
                usage.to_string(),
            ),
            (
                "GET /api/oauth/usage?cedar_ember=1 ",
                if options.fail_refresh { 401 } else { 200 },
                refreshed.to_string(),
            ),
            (
                "GET /api/oauth/profile ",
                profile_status,
                serde_json::json!({"organization":{"uuid": org}}).to_string(),
            ),
            (
                claim_route,
                claim_status,
                if result == "malformed" {
                    "{".to_owned()
                } else {
                    response.to_string()
                },
            ),
        ],
        count,
    );
    (format!("{origin}/api/oauth/usage"), server)
}

fn claude_redeem_command(
    env: &Env,
    usage_url: &str,
    name: &str,
    flag: &str,
) -> std::process::Output {
    let mut command = env.rimz();
    command.args(["accounts", "redeem", "claude"]);
    if !name.is_empty() {
        command.arg(name);
    }
    if !flag.is_empty() {
        command.arg(flag);
    }
    command
        .env("RIMZ_CLAUDE_OAUTH_USAGE_URL", usage_url)
        .env("PATH", path_with_front(&env.home_root.join("bin")))
        .bounded_output()
        .unwrap()
}

fn claude_redeem_stamp(env: &Env, name: &str) -> std::path::PathBuf {
    env.runtime_paths()
        .shared_credits_path()
        .with_file_name(format!("auto_redeem.claude@{name}.json"))
}

#[test]
fn claude_redeem_dry_run_has_two_reads_no_forecast_and_no_durable_attempt() {
    let env = Env::new();
    let (url, server) = claude_redeem_fixture(
        &env,
        claude_redeem_usage(),
        200,
        200,
        "reset",
        2,
        Default::default(),
    );
    let output = claude_redeem_command(&env, &url, "", "--dry-run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout
            .lines()
            .any(|line| line.split_whitespace().take(2).eq(["credits:", "2;"]))
            && stdout.contains("reset stays at"),
        "{stdout}"
    );
    assert!(!stdout.contains("forecast"), "{stdout}");
    assert!(!claude_redeem_stamp(&env, "default").exists());
    assert!(rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None).is_empty());
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(requests.iter().all(|request| request.starts_with("GET ")));
}

#[test]
fn claude_redeem_results_persist_once_and_refresh_only_a_reset() {
    for (result, outcome, exit) in [
        ("reset", "reset", 0),
        ("already_used", "already_redeemed", 5),
        ("not_limited", "nothing_to_reset", 4),
        ("ineligible", "no_credit", 3),
        ("unavailable", "no_credit", 3),
        ("cooldown", "cooldown", 7),
        ("new_result", "unknown", 6),
    ] {
        let env = Env::new();
        let (url, server) = claude_redeem_fixture(
            &env,
            claude_redeem_usage(),
            200,
            200,
            result,
            if result == "reset" { 4 } else { 3 },
            Default::default(),
        );
        let output = claude_redeem_command(&env, &url, "", "--yes");
        assert_eq!(
            output.status.code(),
            Some(exit),
            "{result}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(&format!("claude@default: {outcome}")),
            "{stdout}"
        );
        let stamp = read_json(claude_redeem_stamp(&env, "default"));
        assert_eq!(stamp["outcome"], outcome);
        let records = rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None);
        assert_eq!(records.len(), 1);
        let record = serde_json::to_value(&records[0]).unwrap();
        assert_eq!(record["kind"], "claude");
        assert_eq!(record["reason"], "manual");
        assert_eq!(record["outcome"], outcome);
        assert_eq!(record["windows_reset"], result == "reset");
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), if result == "reset" { 4 } else { 3 });
        assert!(requests[0].starts_with("GET /api/oauth/usage?cedar_ember=1&skip_spend=1 "));
        let claims: Vec<_> = requests
            .iter()
            .filter(|request| request.starts_with("POST "))
            .collect();
        assert_eq!(claims.len(), 1);
        let claim = claims[0];
        assert!(claim.starts_with("POST /api/organizations/profile-org/reset_rate_limits "));
        let headers = claim.split_once("\r\n\r\n").unwrap().0.to_lowercase();
        for header in [
            "authorization: bearer access-default",
            "anthropic-beta: oauth-2025-04-20",
            "user-agent: claude-cli/2.1.173 (external, cli)",
            "content-type: application/json",
            "accept: application/json",
        ] {
            assert!(headers.contains(header), "{headers}");
        }
        let body: Value = serde_json::from_str(claim.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body.as_object().unwrap().len(), 3);
        assert_eq!(body["program"], "cedar_ember");
        assert_eq!(body["grant_id"], "selected");
        assert_eq!(body["request_id"], stamp["request_id"]);
        assert_eq!(body["request_id"], record["request_id"]);
        assert_eq!(
            uuid::Uuid::parse_str(body["request_id"].as_str().unwrap())
                .unwrap()
                .get_version_num(),
            7
        );
        if result == "reset" {
            assert!(requests[3].starts_with("GET /api/oauth/usage?cedar_ember=1 "));
            assert_eq!(
                read_json(env.runtime_paths().shared_credits_path())["logins"]["claude@default"]["extra_credits"]
                    ["known"]["used_usd"],
                7.25
            );
            assert_eq!(
                read_json(env.runtime_paths().shared_credits_path())["logins"]["claude@default"]["reset_credits"]
                    ["count"],
                1
            );
            let output = env
                .rimz()
                .args(["stats", "--json"])
                .bounded_output()
                .unwrap();
            assert!(output.status.success());
            let stats: Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(stats["assists"]["rollup"]["manual_redeems"], 1, "{stats}");
            assert_eq!(stats["assists"]["rollup"]["manual_resets"], 1);
        }
    }
}

#[test]
fn claude_redeem_named_account_uses_only_its_credentials() {
    let env = Env::new();
    let (url, server) = claude_redeem_fixture(
        &env,
        claude_redeem_usage(),
        200,
        200,
        "reset",
        4,
        ClaudeRedeemOptions {
            named_org: true,
            ..Default::default()
        },
    );
    let output = claude_redeem_command(&env, &url, "spare", "--yes");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let requests = server.join().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests[2].starts_with("POST /api/organizations/spare-org/reset_rate_limits "));
    assert!(requests.iter().all(|request| {
        request
            .to_lowercase()
            .contains("authorization: bearer access-spare")
            && !request.contains("access-default")
    }));
    assert_eq!(
        read_json(claude_redeem_stamp(&env, "spare"))["outcome"],
        "reset"
    );
}

#[test]
fn claude_redeem_holds_do_not_reserve_or_record() {
    for (field, value, code, reason) in [
        ("paused", true.into(), 3, "paused"),
        (
            "cooldown_until",
            "2100-01-01T00:00:00Z".into(),
            7,
            "cooldown",
        ),
    ] {
        let env = Env::new();
        let mut usage = claude_redeem_usage();
        if field == "paused" {
            usage["cedar_ember"]["grants"][1][field] = value;
        } else {
            usage["cedar_ember"][field] = value;
        }
        let (url, server) =
            claude_redeem_fixture(&env, usage, 200, 200, "reset", 2, Default::default());
        let output = claude_redeem_command(&env, &url, "", "--yes");
        assert_eq!(
            output.status.code(),
            Some(code),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("hold")
                && stdout.contains(reason)
                && stdout
                    .lines()
                    .any(|line| line.split_whitespace().take(2).eq(["credits:", "2;"])),
            "{stdout}"
        );
        assert!(!claude_redeem_stamp(&env, "default").exists());
        assert!(rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None).is_empty());
        assert_eq!(server.join().unwrap().len(), 2);
    }
}

#[test]
fn claude_redeem_unknown_transport_outcomes_leave_a_live_reservation() {
    for status in [503, 0, 200] {
        let env = Env::new();
        let (url, server) = claude_redeem_fixture(
            &env,
            claude_redeem_usage(),
            200,
            status,
            if status == 200 { "malformed" } else { "reset" },
            5,
            Default::default(),
        );
        let output = claude_redeem_command(&env, &url, "", "--yes");
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("outcome is unknown") && stderr.contains("Settings > Usage"),
            "{stderr}"
        );
        let stamp = read_json(claude_redeem_stamp(&env, "default"));
        assert!(stamp.get("outcome").is_none());
        let records = rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None);
        assert_eq!(records.len(), 1);
        assert!(
            serde_json::to_value(&records[0]).unwrap()["error"]
                .as_str()
                .unwrap()
                .contains("outcome is unknown")
        );
        let retry = claude_redeem_command(&env, &url, "", "--yes");
        assert_eq!(retry.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&retry.stderr).contains("pending attempt"));
        assert_eq!(read_json(claude_redeem_stamp(&env, "default")), stamp);
        let requests = server.join().unwrap();
        assert_eq!(requests.len(), 5);
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.starts_with("POST "))
                .count(),
            1
        );
    }
}

#[test]
fn claude_redeem_profile_and_credential_failures_spend_nothing() {
    for profile_status in [404, 401] {
        let env = Env::new();
        let (url, server) = claude_redeem_fixture(
            &env,
            claude_redeem_usage(),
            profile_status,
            200,
            "reset",
            2,
            Default::default(),
        );
        let output = claude_redeem_command(&env, &url, "", "--yes");
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!(
                "claude OAuth profile HTTP status {profile_status}"
            )),
            "{stderr}"
        );
        assert!(!claude_redeem_stamp(&env, "default").exists());
        assert_eq!(server.join().unwrap().len(), 2);
    }
    let env = Env::new();
    let (url, server) = claude_redeem_fixture(
        &env,
        claude_redeem_usage(),
        200,
        200,
        "reset",
        0,
        Default::default(),
    );
    let path = env.home_root.join(".claude/.credentials.json");
    let mut credentials = read_json(path.clone());
    credentials["claudeAiOauth"]["expiresAt"] = 1.into();
    std::fs::write(path, credentials.to_string()).unwrap();
    let output = claude_redeem_command(&env, &url, "", "--yes");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("token is expired"));
    assert!(!claude_redeem_stamp(&env, "default").exists());
    assert!(server.join().unwrap().is_empty());
}

#[test]
fn claude_redeem_missing_cleared_and_failed_refresh_still_complete_the_claim() {
    let env = Env::new();
    let (url, server) = claude_redeem_fixture(
        &env,
        claude_redeem_usage(),
        200,
        200,
        "reset",
        4,
        ClaudeRedeemOptions {
            omit_cleared: true,
            fail_refresh: true,
            ..Default::default()
        },
    );
    let output = claude_redeem_command(&env, &url, "", "--yes");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("usage refresh failed"));
    assert_eq!(
        read_json(claude_redeem_stamp(&env, "default"))["outcome"],
        "reset"
    );
    let records = rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None);
    assert_eq!(
        serde_json::to_value(&records[0]).unwrap()["windows_reset"],
        false
    );
    assert_eq!(server.join().unwrap().len(), 4);
}

#[test]
fn claude_redeem_live_reservation_and_nonterminal_refuse_without_claiming() {
    let env = Env::new();
    let (url, server) = claude_redeem_fixture(
        &env,
        claude_redeem_usage(),
        200,
        200,
        "reset",
        2,
        Default::default(),
    );
    let output = claude_redeem_command(&env, &url, "", "");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("--yes"));
    let path = claude_redeem_stamp(&env, "default");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::json!({"attempted_at": jiff::Timestamp::now(), "request_id": "pending", "reason": "manual"}).to_string()).unwrap();
    let output = claude_redeem_command(&env, &url, "", "--yes");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("pending attempt"));
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn claude_redeem_declined_terminal_confirmation_spends_nothing() {
    let env = Env::new();
    let (url, server) = claude_redeem_fixture(
        &env,
        claude_redeem_usage(),
        200,
        200,
        "reset",
        2,
        Default::default(),
    );
    let pty = nix::pty::openpty(None, None).unwrap();
    let mut master = std::fs::File::from(pty.master);
    master.write_all(b"n\n").unwrap();
    let output = env
        .rimz()
        .args(["accounts", "redeem", "claude"])
        .stdin(std::process::Stdio::from(pty.slave))
        .env("RIMZ_CLAUDE_OAUTH_USAGE_URL", url)
        .env("PATH", path_with_front(&env.home_root.join("bin")))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("nothing spent"));
    assert!(!claude_redeem_stamp(&env, "default").exists());
    assert!(rimz::harness::assist_log::recent(&env.rimz_home().join("logs"), None).is_empty());
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn claude_redeem_cooldown_uses_local_preview_time() {
    let env = Env::new();
    let mut usage = claude_redeem_usage();
    usage["cedar_ember"]["cooldown_until"] = "2100-01-01T00:00:00Z".into();
    let (url, server) =
        claude_redeem_fixture(&env, usage, 200, 200, "reset", 2, Default::default());
    let output = env
        .rimz()
        .args(["accounts", "redeem", "claude", "--yes"])
        .env("RIMZ_CLAUDE_OAUTH_USAGE_URL", url)
        .env("PATH", path_with_front(&env.home_root.join("bin")))
        .env("TZ", "America/New_York")
        .bounded_output()
        .unwrap();
    assert_eq!(output.status.code(), Some(7));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("cooldown until 2099-12-31 19:00:00 -05:00"),
        "{stdout}"
    );
    assert!(
        stdout.contains("reset stays at 2099-12-31 19:00:00 -05:00"),
        "{stdout}"
    );
    assert!(!claude_redeem_stamp(&env, "default").exists());
    assert_eq!(server.join().unwrap().len(), 2);
}

#[test]
fn accounts_redeem_kind_without_named_accounts_lists_supported_kinds() {
    for kind in ["grok", "claud"] {
        let env = Env::new();
        let output = env
            .rimz()
            .args(["accounts", "redeem", kind, "--dry-run"])
            .bounded_output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(&format!(
                "{kind} has no named accounts; accounts are supported for claude, codex"
            )),
            "{stderr}"
        );
    }
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
