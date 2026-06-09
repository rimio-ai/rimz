use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use serde_json::json;

use super::*;

/// In-memory transport: returns canned results by method and records the
/// call order so the handshake sequence can be asserted.
struct CannedTransport {
    results: HashMap<&'static str, Value>,
    errors: HashSet<&'static str>,
    calls: Vec<String>,
}

impl CannedTransport {
    fn new() -> Self {
        let mut results = HashMap::new();
        results.insert(
            "initialize",
            json!({
                "userAgent": "rimz/0.135.0 (Ubuntu 25.4.0; x86_64) xterm-256color",
                "codexHome": "/home/u/.codex",
                "platformFamily": "unix",
                "platformOs": "linux"
            }),
        );
        Self {
            results,
            errors: HashSet::new(),
            calls: Vec::new(),
        }
    }

    fn with(mut self, method: &'static str, result: Value) -> Self {
        self.results.insert(method, result);
        self
    }

    fn failing(mut self, method: &'static str) -> Self {
        self.errors.insert(method);
        self
    }
}

impl JsonRpcTransport for CannedTransport {
    fn request(&mut self, method: &str, _params: Value) -> Result<Value, AppServerErr> {
        self.calls.push(method.to_owned());
        if self.errors.contains(method) {
            return Err(AppServerErr::JsonRpc {
                code: -32000,
                message: "boom".to_owned(),
            });
        }
        Ok(self.results.get(method).cloned().unwrap_or(Value::Null))
    }

    fn notify(&mut self, method: &str, _params: Value) -> Result<(), AppServerErr> {
        self.calls.push(format!("notify:{method}"));
        Ok(())
    }
}

fn rate_limits_result() -> Value {
    json!({
        "rateLimits": {
            "limitId": "codex",
            "primary": { "usedPercent": 12, "windowDurationMins": 300, "resetsAt": 1_780_092_691_i64 },
            "secondary": { "usedPercent": 88, "windowDurationMins": 10080, "resetsAt": 1_780_186_207_i64 },
            "planType": "team"
        }
    })
}

fn model_list_result() -> Value {
    json!({
        "data": [
            { "id": "gpt-5.5-codex", "model": "gpt-5.5-codex", "displayName": "GPT-5.5 Codex",
              "defaultReasoningEffort": "high", "isDefault": true, "description": "", "hidden": false,
              "supportedReasoningEfforts": [] },
            { "id": "o4-mini", "model": "o4-mini", "displayName": "o4-mini",
              "defaultReasoningEffort": "medium", "isDefault": false, "description": "", "hidden": false,
              "supportedReasoningEfforts": [] }
        ]
    })
}

fn ts() -> Timestamp {
    Timestamp::from_second(1_780_000_000).unwrap()
}

#[test]
fn handshake_initializes_then_acknowledges_before_reads() {
    let transport = CannedTransport::new()
        .with("account/rateLimits/read", rate_limits_result())
        .with("model/list", model_list_result());
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let _ = client.observe_context("codex", None, Some("gpt-5.5-codex"), ts());
    // initialize, then the initialized notification, then the reads.
    assert_eq!(client.transport.calls[0], "initialize");
    assert_eq!(client.transport.calls[1], "notify:initialized");
    assert!(
        client
            .transport
            .calls
            .iter()
            .position(|c| c == "account/rateLimits/read")
            .unwrap()
            > 1
    );
}

#[test]
fn maps_each_window_with_its_duration() {
    let transport = CannedTransport::new().with("account/rateLimits/read", rate_limits_result());
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let ctx = client.observe_context("codex", None, None, ts());
    let limits = ctx.rate_limits.expect("rate limits present");
    assert_eq!(limits.windows.len(), 2);
    // Wire order is preserved: primary (300 min) then secondary (10080 min).
    let five = &limits.windows[0];
    let seven = &limits.windows[1];
    assert_eq!(five.duration_mins, Some(300));
    assert_eq!(five.used_percentage, Some(12));
    assert_eq!(five.resets_at, Timestamp::from_second(1_780_092_691).ok());
    assert_eq!(seven.duration_mins, Some(10080));
    assert_eq!(seven.used_percentage, Some(88));
    assert_eq!(seven.resets_at, Timestamp::from_second(1_780_186_207).ok());
}

#[test]
fn single_window_maps_to_one_window() {
    // A single window — `primary` with `secondary: null` — maps to one window
    // carrying its own duration, beside fields the tolerant wire model ignores.
    // (This is the shape a transient Codex server bug produced, widening the
    // window to ~30 days; the mapper carries whatever the wire reports.)
    let result = json!({
        "rateLimits": {
            "limitId": "codex",
            "limitName": null,
            "primary": { "usedPercent": 0, "windowDurationMins": 43800, "resetsAt": 1_783_005_867_i64 },
            "secondary": null,
            "credits": { "hasCredits": false, "unlimited": false, "balance": null },
            "planType": "team",
            "rateLimitReachedType": null
        }
    });
    let transport = CannedTransport::new().with("account/rateLimits/read", result);
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let ctx = client.observe_context("codex", None, None, ts());
    let limits = ctx.rate_limits.expect("rate limits present");
    assert_eq!(limits.windows.len(), 1, "one window, no secondary");
    let window = &limits.windows[0];
    assert_eq!(window.duration_mins, Some(43800));
    assert_eq!(window.used_percentage, Some(0));
    assert_eq!(window.resets_at, Timestamp::from_second(1_783_005_867).ok());
    assert_eq!(ctx.account.unwrap().plan.as_deref(), Some("team"));
}

#[test]
fn account_plan_type_rides_the_rate_limit_read() {
    // The plan tier sits beside the windows in `account/rateLimits/read`, so
    // it lands on the context account — metered, since a tier is present.
    let transport = CannedTransport::new().with("account/rateLimits/read", rate_limits_result());
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let account = client
        .observe_context("codex", None, None, ts())
        .account
        .expect("account from planType");
    assert_eq!(account.plan.as_deref(), Some("team"));
    assert_eq!(account.metered, Some(true));
}

#[test]
fn api_key_account_leaves_account_none() {
    // No windows and no plan tier (an API-key account) means no account —
    // the dashboard then infers the unmetered "infinite" bar.
    let result = json!({ "rateLimits": {} });
    let transport = CannedTransport::new().with("account/rateLimits/read", result);
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    assert_eq!(
        client.observe_context("codex", None, None, ts()).account,
        None
    );
}

#[test]
fn windows_without_duration_keep_order_and_carry_no_duration() {
    let result = json!({
        "rateLimits": {
            "primary": { "usedPercent": 5 },
            "secondary": { "usedPercent": 50 }
        }
    });
    let transport = CannedTransport::new().with("account/rateLimits/read", result);
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let limits = client
        .observe_context("codex", None, None, ts())
        .rate_limits
        .expect("rate limits");
    assert_eq!(limits.windows.len(), 2);
    assert_eq!(limits.windows[0].used_percentage, Some(5));
    assert_eq!(limits.windows[0].duration_mins, None);
    assert_eq!(limits.windows[1].used_percentage, Some(50));
    assert_eq!(limits.windows[1].duration_mins, None);
}

#[test]
fn used_percent_is_clamped() {
    let result = json!({
        "rateLimits": { "primary": { "usedPercent": 250, "windowDurationMins": 300 },
                        "secondary": { "usedPercent": -5, "windowDurationMins": 10080 } }
    });
    let transport = CannedTransport::new().with("account/rateLimits/read", result);
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let limits = client
        .observe_context("codex", None, None, ts())
        .rate_limits
        .unwrap();
    assert_eq!(limits.windows[0].used_percentage, Some(100));
    assert_eq!(limits.windows[1].used_percentage, Some(0));
}

#[test]
fn model_hint_resolves_display_name_but_not_default_effort() {
    let transport = CannedTransport::new().with("model/list", model_list_result());
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let ctx = client.observe_context("codex", None, Some("gpt-5.5-codex"), ts());
    assert_eq!(ctx.model_id.as_deref(), Some("gpt-5.5-codex"));
    assert_eq!(ctx.model_display_name.as_deref(), Some("GPT-5.5 Codex"));
    assert_eq!(
        ctx.effort, None,
        "model/list defaultReasoningEffort is a recommendation, not the session's actual effort"
    );
    assert_eq!(ctx.agent_version.as_deref(), Some("0.135.0"));
}

#[test]
fn thread_read_preview_and_name_land_on_context() {
    let transport = CannedTransport::new().with(
        "thread/read",
        json!({
            "thread": {
                "id": "sess-1",
                "preview": "Create a TUI",
                "name": "TUI prototype"
            }
        }),
    );
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let ctx = client.observe_context("codex", Some("sess-1"), None, ts());

    assert_eq!(ctx.session_preview.as_deref(), Some("Create a TUI"));
    assert_eq!(ctx.session_name.as_deref(), Some("TUI prototype"));
    assert!(
        !client.transport.calls.iter().any(|c| c == "thread/list"),
        "a direct read with preview does not need the list fallback"
    );
}

#[test]
fn thread_list_preview_fills_when_read_has_only_name() {
    let transport = CannedTransport::new()
        .with(
            "thread/read",
            json!({
                "thread": {
                    "id": "sess-1",
                    "name": "TUI prototype"
                }
            }),
        )
        .with(
            "thread/list",
            json!({
                "data": [
                    { "id": "older", "preview": "Ignore me" },
                    { "id": "thread-fork", "sessionId": "sess-1", "preview": "Create a TUI" }
                ]
            }),
        );
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let ctx = client.observe_context("codex", Some("sess-1"), None, ts());

    assert_eq!(ctx.session_preview.as_deref(), Some("Create a TUI"));
    assert_eq!(
        ctx.session_name.as_deref(),
        Some("TUI prototype"),
        "the direct title is preserved when the list supplies only preview"
    );
}

#[test]
fn unmatched_or_absent_model_hint_leaves_model_none() {
    let transport = CannedTransport::new().with("model/list", model_list_result());
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let unmatched = client.observe_context("codex", None, Some("does-not-exist"), ts());
    assert_eq!(unmatched.model_display_name, None);
    assert_eq!(unmatched.model_id, None);

    let absent = client.observe_context("codex", None, None, ts());
    assert_eq!(absent.model_display_name, None);
    assert_eq!(absent.effort, None);
}

#[test]
fn rate_limit_read_error_still_yields_version_and_model() {
    let transport = CannedTransport::new()
        .failing("account/rateLimits/read")
        .with("model/list", model_list_result());
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let ctx = client.observe_context("codex", None, Some("o4-mini"), ts());
    assert_eq!(ctx.rate_limits, None);
    assert_eq!(ctx.model_display_name.as_deref(), Some("o4-mini"));
    assert_eq!(ctx.agent_version.as_deref(), Some("0.135.0"));
    assert_eq!(ctx.source, "codex");
}

#[test]
fn version_parsed_from_user_agent_token() {
    assert_eq!(
        codex_version_from_user_agent("rimz/0.135.0 (Ubuntu 25.4.0; x86_64)").as_deref(),
        Some("0.135.0")
    );
    assert_eq!(codex_version_from_user_agent("nogap").as_deref(), None);
    assert_eq!(codex_version_from_user_agent("trailing/").as_deref(), None);
}

#[test]
fn loaded_threads_reads_the_id_list_after_handshake() {
    let transport =
        CannedTransport::new().with("thread/loaded/list", json!({ "threadIds": ["t-1", "t-2"] }));
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    assert_eq!(client.loaded_threads().unwrap(), ["t-1", "t-2"]);
    // The list read rides after the handshake, like every other method.
    assert_eq!(client.transport.calls[0], "initialize");
    assert_eq!(client.transport.calls[1], "notify:initialized");
    assert!(
        client
            .transport
            .calls
            .iter()
            .any(|c| c == "thread/loaded/list")
    );
}

#[test]
fn parse_loaded_threads_accepts_known_shapes() {
    // Primary: a flat id list under `threadIds`.
    assert_eq!(
        parse_loaded_threads(&json!({ "threadIds": ["a", "b"] })).unwrap(),
        ["a", "b"]
    );
    // Tolerated key alias.
    assert_eq!(
        parse_loaded_threads(&json!({ "threads": ["a"] })).unwrap(),
        ["a"]
    );
    // Id-bearing objects, with an empty id dropped.
    assert_eq!(
        parse_loaded_threads(&json!({ "threadIds": [{ "id": "a" }, { "threadId": "b" }, ""] }))
            .unwrap(),
        ["a", "b"]
    );
    // A bare array is accepted too.
    assert_eq!(
        parse_loaded_threads(&json!(["a", "b"])).unwrap(),
        ["a", "b"]
    );
}

#[test]
fn parse_loaded_threads_trusts_an_empty_known_list() {
    // A recognized but empty list is genuinely "zero loaded" — trusted, so the
    // caller may reap absent sessions against it.
    assert!(
        parse_loaded_threads(&json!({ "threadIds": [] }))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn parse_loaded_threads_errors_on_unrecognized_shape() {
    // No recognized id field → untrusted → error, so the caller keeps every
    // session rather than reaping against a shape it could not read.
    assert!(parse_loaded_threads(&json!({ "foo": 1 })).is_err());
    assert!(parse_loaded_threads(&json!({})).is_err());
    assert!(parse_loaded_threads(&Value::Null).is_err());
}

#[test]
fn parse_loaded_threads_errors_on_a_nonempty_unreadable_array() {
    // A recognized key holding a non-empty array we can read no id from is a
    // wire-shape drift, not "zero loaded" — untrusted, so the caller keeps
    // every session rather than mass-reaping against a list it never read. An
    // empty `[]` stays trusted-zero (see the test above); the discriminator is
    // strictly empty-vs-unreadable.
    assert!(parse_loaded_threads(&json!({ "threadIds": [{ "sessionId": "x" }] })).is_err());
    assert!(parse_loaded_threads(&json!({ "threadIds": [""] })).is_err());
    assert!(parse_loaded_threads(&json!([{ "weird": 1 }])).is_err());
}

/// Assert a `Spawn` attempt's argv + budget; panics on a `Broker` attempt.
fn assert_spawn(attempt: &ConnectAttempt, args: &[&str], deadline: Duration) {
    match attempt {
        ConnectAttempt::Spawn(got_args, got_deadline) => {
            assert_eq!(
                got_args,
                &args.iter().map(|s| s.to_string()).collect::<Vec<_>>()
            );
            assert_eq!(*got_deadline, deadline);
        }
        ConnectAttempt::Broker(path) => panic!("expected a spawn attempt, got broker {path:?}"),
    }
}

#[test]
fn attempts_cold_spawn_only_without_any_socket() {
    let attempts = attempts_for(None, None);
    assert_eq!(attempts.len(), 1, "no broker, no daemon → cold-spawn only");
    assert_spawn(&attempts[0], &["app-server"], APP_SERVER_DEADLINE);
}

#[test]
fn attempts_prefer_daemon_proxy_then_cold_spawn() {
    let sock = Path::new("/run/codex/app-server-control.sock");
    let attempts = attempts_for(None, Some(sock));
    assert_eq!(attempts.len(), 2, "daemon → proxy then fallback");
    assert_spawn(
        &attempts[0],
        &[
            "app-server",
            "proxy",
            "--sock",
            "/run/codex/app-server-control.sock",
        ],
        PROXY_PROBE_DEADLINE,
    );
    assert_spawn(&attempts[1], &["app-server"], APP_SERVER_DEADLINE);
}

#[test]
fn attempts_prefer_broker_first_then_daemon_then_cold_spawn() {
    let broker = Path::new("/run/user/1000/rimz/w/sock/codex-app-server.sock");
    let daemon = Path::new("/run/codex/app-server-control.sock");
    let attempts = attempts_for(Some(broker), Some(daemon));
    assert_eq!(attempts.len(), 3, "broker → daemon proxy → cold-spawn");
    match &attempts[0] {
        ConnectAttempt::Broker(path) => assert_eq!(path, broker),
        other => panic!("broker must come first, got a spawn attempt: {other:?}"),
    }
    assert_spawn(
        &attempts[1],
        &[
            "app-server",
            "proxy",
            "--sock",
            "/run/codex/app-server-control.sock",
        ],
        PROXY_PROBE_DEADLINE,
    );
    assert_spawn(&attempts[2], &["app-server"], APP_SERVER_DEADLINE);
}

#[test]
fn attempts_broker_then_cold_spawn_without_a_daemon() {
    let broker = Path::new("/run/user/1000/rimz/w/sock/codex-app-server.sock");
    let attempts = attempts_for(Some(broker), None);
    assert_eq!(attempts.len(), 2, "broker → cold-spawn (no daemon)");
    match &attempts[0] {
        ConnectAttempt::Broker(path) => assert_eq!(path, broker),
        other => panic!("broker must come first, got a spawn attempt: {other:?}"),
    }
    assert_spawn(&attempts[1], &["app-server"], APP_SERVER_DEADLINE);
}
