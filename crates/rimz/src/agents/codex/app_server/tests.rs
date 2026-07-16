use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use serde_json::json;

use super::*;
use crate::agents::{
    AgentCost, AgentCurrentUsage, AgentTokenUsage, LocalContextRefresh, RateLimitWindow,
    TranscriptStat,
};
use crate::{RuntimePaths, WorkspaceId};

struct CannedTransport {
    results: HashMap<&'static str, Value>,
    sequences: HashMap<&'static str, Vec<Value>>,
    errors: HashSet<&'static str>,
    calls: Vec<String>,
    params: Vec<Value>,
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
            sequences: HashMap::new(),
            errors: HashSet::new(),
            calls: Vec::new(),
            params: Vec::new(),
        }
    }

    fn with(mut self, method: &'static str, result: Value) -> Self {
        self.results.insert(method, result);
        self
    }

    fn with_sequence(mut self, method: &'static str, results: Vec<Value>) -> Self {
        self.sequences.insert(method, results);
        self
    }

    fn failing(mut self, method: &'static str) -> Self {
        self.errors.insert(method);
        self
    }
}

impl JsonRpcTransport for CannedTransport {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, AppServerErr> {
        self.calls.push(method.to_owned());
        self.params.push(params);
        if self.errors.contains(method) {
            return Err(AppServerErr::JsonRpc {
                code: -32000,
                message: "boom".to_owned(),
            });
        }
        if let Some(results) = self.sequences.get_mut(method)
            && !results.is_empty()
        {
            return Ok(results.remove(0));
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
            "planType": " team "
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
fn rate_limits_and_account_shapes_map_tolerantly() {
    let transport = CannedTransport::new().with("account/rateLimits/read", rate_limits_result());
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let ctx = client.observe_context("codex", None, None, ts());
    let limits = ctx.rate_limits.expect("rate limits present");
    assert_eq!(limits.windows.len(), 2);
    assert_eq!(limits.windows[0].duration_mins, Some(300));
    assert_eq!(limits.windows[0].used_percentage, Some(12));
    assert_eq!(
        limits.windows[0].resets_at,
        Timestamp::from_second(1_780_092_691).ok()
    );
    assert_eq!(limits.windows[1].duration_mins, Some(10080));
    assert_eq!(limits.windows[1].used_percentage, Some(88));
    let account = ctx.account.expect("account from planType");
    assert_eq!(account.plan.as_deref(), Some("team"));
    assert_eq!(account.metered, Some(true));

    let result = json!({
        "rateLimits": {
            "primary": { "usedPercent": 0, "windowDurationMins": 43800, "resetsAt": 1_783_005_867_i64 },
            "secondary": null,
            "planType": "team"
        }
    });
    let transport = CannedTransport::new().with("account/rateLimits/read", result);
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let ctx = client.observe_context("codex", None, None, ts());
    let limits = ctx.rate_limits.expect("single window");
    assert_eq!(limits.windows.len(), 2);
    assert_eq!(limits.windows[0].duration_mins, Some(300));
    assert!(limits.windows[0].lifted);
    assert_eq!(limits.windows[1].duration_mins, Some(43800));
    assert_eq!(limits.windows[1].used_percentage, Some(0));
    assert_eq!(
        limits.windows[1].resets_at,
        Timestamp::from_second(1_783_005_867).ok()
    );

    let result = json!({
        "rateLimits": {
            "primary": { "usedPercent": 250 },
            "secondary": { "usedPercent": -5 }
        }
    });
    let transport = CannedTransport::new().with("account/rateLimits/read", result);
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let limits = client
        .observe_context("codex", None, None, ts())
        .rate_limits
        .unwrap();
    assert_eq!(limits.windows[0].used_percentage, Some(100));
    assert_eq!(limits.windows[0].duration_mins, None);
    assert_eq!(limits.windows[1].used_percentage, Some(0));
    assert_eq!(limits.windows[1].duration_mins, None);

    let transport =
        CannedTransport::new().with("account/rateLimits/read", json!({ "rateLimits": {} }));
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let ctx = client.observe_context("codex", None, None, ts());
    assert_eq!(ctx.account, None);
    assert_eq!(ctx.rate_limits, None);
}

#[test]
fn rate_limits_response_maps_credits_balance_at_root_or_inside_rate_limits() {
    for (label, result, expected) in [
        (
            "root credits",
            json!({
                "rateLimits": {},
                "credits": { "balance": 12.5 }
            }),
            ExtraCredits::known(None, Some(12.5), None),
        ),
        (
            "nested credits",
            json!({
                "rateLimits": {
                    "credits": { "balance": "7.25" }
                }
            }),
            ExtraCredits::known(None, Some(7.25), None),
        ),
    ] {
        let transport = CannedTransport::new().with("account/rateLimits/read", result);
        let mut client = CodexAppServer::new(transport);
        client.handshake().unwrap();
        let observation = client.observe("codex", None, None, ts());
        let credits = observation
            .extra_credits
            .unwrap_or_else(|| panic!("missing credits for {label}"));
        assert_eq!(credits, expected, "{label}");
    }

    let transport = CannedTransport::new().with(
        "account/rateLimits/read",
        json!({
            "rateLimits": {},
            "credits": { "balance": "not money" }
        }),
    );
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    assert_eq!(
        client.observe("codex", None, None, ts()).extra_credits,
        None
    );

    let transport = CannedTransport::new().with(
        "account/rateLimits/read",
        json!({
            "rateLimits": {
                "primary": { "usedPercent": 42, "windowDurationMins": 300 }
            },
            "credits": { "balance": true }
        }),
    );
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let observation = client.observe("codex", None, None, ts());
    assert_eq!(observation.extra_credits, None);
    assert_eq!(
        observation
            .context
            .rate_limits
            .expect("windows survive malformed credits")
            .windows[0]
            .used_percentage,
        Some(42)
    );
}

#[test]
fn rate_limits_response_maps_credit_state_fields() {
    for (label, result, expected) in [
        (
            "disabled camelCase",
            json!({
                "rateLimits": {},
                "credits": { "hasCredits": false }
            }),
            ExtraCredits::Disabled,
        ),
        (
            "disabled snake_case",
            json!({
                "rateLimits": {},
                "credits": { "has_credits": false }
            }),
            ExtraCredits::Disabled,
        ),
        (
            "unlimited",
            json!({
                "rateLimits": {},
                "credits": { "unlimited": true }
            }),
            ExtraCredits::known(None, None, None),
        ),
        (
            "exhausted",
            json!({
                "rateLimits": {},
                "credits": { "overageLimitReached": true, "balance": 12.5 }
            }),
            ExtraCredits::known(None, Some(0.0), None),
        ),
    ] {
        let transport = CannedTransport::new().with("account/rateLimits/read", result);
        let mut client = CodexAppServer::new(transport);
        client.handshake().unwrap();
        assert_eq!(
            client.observe("codex", None, None, ts()).extra_credits,
            Some(expected),
            "{label}"
        );
    }

    let transport = CannedTransport::new().with(
        "account/rateLimits/read",
        json!({
            "rateLimits": {
                "credits": { "hasCredits": false }
            },
            "credits": { "balance": true }
        }),
    );
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    assert_eq!(
        client.observe("codex", None, None, ts()).extra_credits,
        Some(ExtraCredits::Disabled)
    );
}

#[test]
fn rate_limits_response_maps_reset_credit_summary() {
    let transport = CannedTransport::new().with(
        "account/rateLimits/read",
        json!({
            "rateLimits": {},
            "rateLimitResetCredits": {
                "availableCount": 3,
                "credits": [
                    { "status": "available", "expiresAt": 1_780_000_200_i64 },
                    { "status": "redeemed", "expiresAt": 1_780_000_100_i64 },
                    { "status": "available", "expiresAt": 1_780_000_300_i64 }
                ]
            }
        }),
    );
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    assert_eq!(
        client.observe("codex", None, None, ts()).reset_credits,
        Some(ResetCredits {
            count: 3,
            soonest_expiry: Timestamp::from_second(1_780_000_200).ok(),
        })
    );

    let transport = CannedTransport::new().with(
        "account/rateLimits/read",
        json!({
            "rateLimits": {},
            "rateLimitResetCredits": { "availableCount": 0, "credits": null }
        }),
    );
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    assert_eq!(
        client.observe("codex", None, None, ts()).reset_credits,
        Some(ResetCredits {
            count: 0,
            soonest_expiry: None,
        })
    );
}

#[test]
fn context_enrichment_reads_model_thread_version_and_survives_partial_failures() {
    let transport = CannedTransport::new().with("model/list", model_list_result());
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let ctx = client.observe_context("codex", None, Some("gpt-5.5-codex"), ts());
    assert_eq!(ctx.model_id.as_deref(), Some("gpt-5.5-codex"));
    assert_eq!(ctx.model_display_name.as_deref(), Some("GPT-5.5 Codex"));
    assert_eq!(ctx.effort, None);
    assert_eq!(ctx.agent_version.as_deref(), Some("0.135.0"));

    let transport = CannedTransport::new().with(
        "thread/read",
        json!({
            "thread": { "id": "sess-1", "preview": "Create a TUI", "name": "TUI prototype" }
        }),
    );
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    let ctx = client.observe_context("codex", Some("sess-1"), None, ts());
    assert_eq!(ctx.session_preview.as_deref(), Some("Create a TUI"));
    assert_eq!(ctx.session_name.as_deref(), Some("TUI prototype"));
    assert!(
        !client.transport.calls.iter().any(|c| c == "thread/list"),
        "direct preview skips the list fallback"
    );

    let transport = CannedTransport::new()
        .with(
            "thread/read",
            json!({ "thread": { "id": "sess-1", "name": "TUI prototype" } }),
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
    assert_eq!(ctx.session_name.as_deref(), Some("TUI prototype"));

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

    let transport = CannedTransport::new().with("model/list", model_list_result());
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    assert_eq!(
        client
            .observe_context("codex", None, Some("does-not-exist"), ts())
            .model_display_name,
        None
    );
    assert_eq!(
        client
            .observe_context("codex", None, None, ts())
            .model_display_name,
        None
    );
}

#[test]
fn loaded_thread_parser_accepts_known_shapes_and_errors_on_drift() {
    let transport =
        CannedTransport::new().with("thread/loaded/list", json!({ "data": ["t-1", "t-2"] }));
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();
    assert_eq!(client.loaded_threads().unwrap(), ["t-1", "t-2"]);
    assert!(
        client
            .transport
            .calls
            .iter()
            .any(|c| c == "thread/loaded/list")
    );
    assert_eq!(client.transport.params[1], json!({}));

    for (payload, expected) in [
        (json!({ "data": ["a", "b"] }), vec!["a", "b"]),
        (json!({ "threadIds": ["a", "b"] }), vec!["a", "b"]),
        (json!({ "threads": ["a"] }), vec!["a"]),
        (
            json!({ "threadIds": [{ "id": "a" }, { "threadId": "b" }, ""] }),
            vec!["a", "b"],
        ),
        (json!(["a", "b"]), vec!["a", "b"]),
    ] {
        assert_eq!(parse_loaded_threads(&payload).unwrap().0, expected);
    }
    assert!(
        parse_loaded_threads(&json!({ "data": [], "nextCursor": null }))
            .unwrap()
            .0
            .is_empty()
    );
    assert_eq!(
        parse_loaded_threads(&json!({ "data": ["a"], "nextCursor": "next" }))
            .unwrap()
            .1
            .as_deref(),
        Some("next")
    );

    for payload in [
        json!({ "foo": 1 }),
        json!({}),
        Value::Null,
        json!({ "threadIds": [{ "sessionId": "x" }] }),
        json!({ "threadIds": [""] }),
        json!([{ "weird": 1 }]),
    ] {
        assert!(parse_loaded_threads(&payload).is_err(), "{payload}");
    }
}

#[test]
fn loaded_threads_follows_next_cursor_pages() {
    let transport = CannedTransport::new().with_sequence(
        "thread/loaded/list",
        vec![
            json!({ "data": ["a"], "nextCursor": "page-2" }),
            json!({ "data": ["b"], "nextCursor": null }),
        ],
    );
    let mut client = CodexAppServer::new(transport);
    client.handshake().unwrap();

    assert_eq!(client.loaded_threads().unwrap(), ["a", "b"]);
    assert_eq!(client.transport.params[1], json!({}));
    assert_eq!(client.transport.params[2], json!({ "cursor": "page-2" }));
}

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
        ConnectAttempt::DaemonWs(path) => {
            panic!("expected a spawn attempt, got daemon websocket {path:?}")
        }
    }
}

fn assert_daemon_ws(attempt: &ConnectAttempt, expected: &Path) {
    match attempt {
        ConnectAttempt::DaemonWs(path) => assert_eq!(path, expected),
        other => panic!("expected daemon websocket attempt, got {other:?}"),
    }
}

#[test]
fn connection_attempts_prefer_warm_paths_before_cold_spawn() {
    let attempts = attempts_for(None, None);
    assert_eq!(attempts.len(), 1);
    assert_spawn(&attempts[0], &["app-server"], APP_SERVER_DEADLINE);

    let daemon = Path::new("/run/codex/app-server-control.sock");
    let attempts = attempts_for(None, Some(daemon));
    assert_eq!(attempts.len(), 2);
    assert_daemon_ws(&attempts[0], daemon);
    assert_spawn(&attempts[1], &["app-server"], APP_SERVER_DEADLINE);

    let broker = Path::new("/run/user/1000/rimz/w/sock/codex-app-server.sock");
    let attempts = attempts_for(Some(broker), Some(daemon));
    assert_eq!(attempts.len(), 3);
    match &attempts[0] {
        ConnectAttempt::Broker(path) => assert_eq!(path, broker),
        other => panic!("broker must come first, got {other:?}"),
    }
    assert_daemon_ws(&attempts[1], daemon);
    assert_spawn(&attempts[2], &["app-server"], APP_SERVER_DEADLINE);

    let attempts = attempts_for(Some(broker), None);
    assert_eq!(attempts.len(), 2);
    match &attempts[0] {
        ConnectAttempt::Broker(path) => assert_eq!(path, broker),
        other => panic!("broker must come first, got {other:?}"),
    }
    assert_spawn(&attempts[1], &["app-server"], APP_SERVER_DEADLINE);
}

#[test]
fn app_server_due_uses_app_server_stamp_not_whole_sidecar() {
    let now = Timestamp::now();
    let mut record = crate::store::agent_context::new_record(
        "codex",
        "sess-1",
        crate::store::agent_context::empty_context("codex", now),
    );
    assert!(app_server_due(None, REFRESH_THROTTLE_SECS));
    assert!(
        app_server_due(Some(&record), REFRESH_THROTTLE_SECS),
        "a fresh transcript-only sidecar has no app-server stamp and is due"
    );

    record.rate_limits_observed_at = Some(now);
    assert!(!app_server_due(Some(&record), REFRESH_THROTTLE_SECS));

    record.rate_limits_observed_at =
        Some(Timestamp::from_second(now.as_second() - REFRESH_THROTTLE_SECS - 1).unwrap());
    assert!(app_server_due(Some(&record), REFRESH_THROTTLE_SECS));
}

#[test]
fn app_server_merge_preserves_transcript_owned_fields() {
    let (_dir, runtime) = runtime();
    seed_transcript_context(&runtime);
    let app_at = Timestamp::from_second(1_700_000_050).unwrap();
    merge_app_server_context(&runtime, "sess-1", app_server_context(app_at)).unwrap();
    assert_merged_context(&runtime, app_at);
}

fn runtime() -> (tempfile::TempDir, RuntimePaths) {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    (dir, runtime)
}

fn seed_transcript_context(runtime: &RuntimePaths) {
    let transcript_at = Timestamp::from_second(1_700_000_000).unwrap();
    crate::store::agent_context::merge_local_context(
        runtime,
        "codex",
        "sess-1",
        LocalContextRefresh {
            model_id: Some("gpt-5".to_owned()),
            model_display_name: None,
            effort: Some("xhigh".to_owned()),
            tokens: Some(transcript_tokens()),
            cost: Some(AgentCost {
                total_cost_usd: Some(0.42),
                ..AgentCost::default()
            }),
            turn_error: None,
            turn_complete: None,
            plan_proposed: None,
            native_permission_wait: None,
            turn_interrupted: None,
            transcript_path: Some("/tmp/rollout.jsonl".to_owned()),
            transcript_stat: Some(TranscriptStat {
                mtime_secs: 10,
                mtime_nanos: 20,
                len: 30,
                companion: None,
            }),
        },
        transcript_at,
    )
    .unwrap();
}

fn transcript_tokens() -> AgentTokenUsage {
    AgentTokenUsage {
        context_window_size: Some(1000),
        used_percentage: Some(25),
        remaining_percentage: Some(75),
        current_context_tokens: None,
        current_usage: Some(AgentCurrentUsage {
            input_tokens: Some(200),
            output_tokens: Some(50),
            cache_creation_input_tokens: None,
            cache_read_input_tokens: Some(50),
        }),
        session_usage: None,
    }
}

fn app_server_context(app_at: Timestamp) -> AgentContext {
    AgentContext {
        source: "codex".to_owned(),
        session_name: Some("TUI prototype".to_owned()),
        session_preview: Some("Create a TUI".to_owned()),
        model_id: Some("gpt-5".to_owned()),
        model_display_name: Some("GPT-5".to_owned()),
        effort: Some("high".to_owned()),
        thinking_enabled: None,
        output_style: None,
        vim_mode: None,
        agent_version: Some("1.2.3".to_owned()),
        exceeds_200k_tokens: None,
        cost: None,
        tokens: None,
        rate_limits: Some(AgentRateLimits {
            windows: vec![RateLimitWindow {
                used_percentage: Some(55),
                resets_at: None,
                duration_mins: Some(300),
                ..Default::default()
            }],
        }),
        pr: None,
        account: Some(AgentAccount {
            scope: Default::default(),
            plan: Some("pro".to_owned()),
            account_id: None,
            metered: Some(true),
            version: None,
            sub_provider: None,
            credentials_updated_at_ms: None,
        }),
        turn_opened_by: Vec::new(),
        turn_error: None,
        turn_complete: None,
        plan_proposed: None,
        native_permission_wait: None,
        turn_interrupted: None,
        observed_at: app_at,
    }
}

fn assert_merged_context(runtime: &RuntimePaths, app_at: Timestamp) {
    let merged = crate::store::agent_context::read_one(runtime, "codex", "sess-1").unwrap();
    assert_eq!(
        merged
            .context
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.used_percentage),
        Some(25)
    );
    assert_eq!(
        merged
            .context
            .cost
            .as_ref()
            .and_then(|cost| cost.total_cost_usd),
        Some(0.42)
    );
    assert_eq!(
        merged.transcript_path.as_deref(),
        Some("/tmp/rollout.jsonl")
    );
    assert_eq!(merged.context.model_display_name.as_deref(), Some("GPT-5"));
    assert_eq!(
        merged.context.session_preview.as_deref(),
        Some("Create a TUI")
    );
    assert_eq!(
        merged.context.session_name.as_deref(),
        Some("TUI prototype")
    );
    assert_eq!(merged.context.effort.as_deref(), Some("xhigh"));
    assert_eq!(
        merged
            .context
            .rate_limits
            .as_ref()
            .and_then(|limits| limits.windows.first())
            .and_then(|window| window.used_percentage),
        Some(55)
    );
    assert_eq!(merged.rate_limits_observed_at, Some(app_at));
}
