use std::collections::BTreeMap;
use std::io::Write;
use std::time::{Duration, SystemTime};

use super::*;
use crate::RuntimePaths;
use crate::ids::WorkspaceId;
use crate::sidebar::test_support::{provider_panel, rl_window, root_agent, snapshot_with_panels};

#[test]
fn live_session_refreshes_target_live_root_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());

    let mut active_with_windows = snapshot_with_panels(
        workspace.clone(),
        vec![provider_panel("codex", vec![rl_window(42, None)])],
    );
    active_with_windows
        .agents
        .push(root_agent("codex", "sess-active", Some("gpt-5.5-codex")));
    active_with_windows
        .agents
        .push(root_agent("claude", "claude-active", Some("opus")));
    assert_eq!(
        live_session_refreshes(&active_with_windows),
        vec![
            LiveSessionRefresh {
                kind: "codex".to_owned(),
                session_id: "sess-active".to_owned(),
                model_hint: Some("gpt-5.5-codex".to_owned()),
            },
            LiveSessionRefresh {
                kind: "claude".to_owned(),
                session_id: "claude-active".to_owned(),
                model_hint: Some("opus".to_owned()),
            }
        ],
        "live root sessions refresh their sidecars even when the dashboard already has windows"
    );

    // An idle metered account has no live session to refresh here — the
    // uniform usage driver covers its account-scoped read while idle.
    let idle_metered =
        snapshot_with_panels(workspace.clone(), vec![provider_panel("codex", Vec::new())]);
    assert!(
        live_session_refreshes(&idle_metered).is_empty(),
        "an idle account has no session sidecar to refresh"
    );

    let mut active_no_model =
        snapshot_with_panels(workspace, vec![provider_panel("codex", Vec::new())]);
    active_no_model
        .agents
        .push(root_agent("codex", "sess-active", None));
    assert_eq!(
        live_session_refreshes(&active_no_model),
        vec![LiveSessionRefresh {
            kind: "codex".to_owned(),
            session_id: "sess-active".to_owned(),
            model_hint: None,
        }],
        "a live sidecar refreshes even with no model hint"
    );
}

/// The per-session throttle marker gates the app-server refresh: the first call
/// is due (and touches the marker), the immediate next is not.
#[test]
fn session_probe_throttles_per_kind_and_session() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    assert!(session_probe_due(&runtime, "codex", "sess/one"));
    assert!(
        !session_probe_due(&runtime, "codex", "sess/one"),
        "a freshly-stamped session backs off"
    );
    assert!(
        session_probe_due(&runtime, "codex", "sess/two"),
        "a different session has its own marker"
    );
    assert!(
        session_probe_due(&runtime, "claude", "sess/one"),
        "a different kind has its own marker"
    );

    let old = SystemTime::now()
        .checked_sub(SESSION_REFRESH_INTERVAL + Duration::from_secs(1))
        .unwrap();
    std::fs::File::open(session_probe_marker(&runtime, "codex", "sess/one"))
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert!(
        session_probe_due(&runtime, "codex", "sess/one"),
        "the session becomes due again after the 60s interval"
    );
}

#[test]
fn reap_removes_stale_session_probe_markers() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    let stale_session = runtime.shared_root.join(format!(
        "{SESSION_PROBE_MARKER_PREFIX}00000000000000000000000000000000"
    ));
    let fresh_session = runtime.shared_root.join(format!(
        "{SESSION_PROBE_MARKER_PREFIX}11111111111111111111111111111111"
    ));
    let accounts = runtime.shared_root.join("accounts.json");
    for path in [&stale_session, &fresh_session, &accounts] {
        std::fs::write(path, b"").unwrap();
    }
    let old = SystemTime::now()
        .checked_sub(SESSION_PROBE_MARKER_TTL + Duration::from_secs(1))
        .unwrap();
    std::fs::File::open(&stale_session)
        .unwrap()
        .set_modified(old)
        .unwrap();

    reap_stale_session_probe_markers(&runtime);

    assert!(!stale_session.exists());
    assert!(fresh_session.exists());
    assert!(accounts.exists());
}

#[test]
fn codex_turn_death_without_pane_infers_spent_window() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let now = write_spent_codex_window(&runtime);
    let mut error = crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::Unknown,
        at: now,
        label: Some("turn ended with no final message".to_owned()),
    };

    confirm_codex_turn_death_from_pane(&runtime, None, &mut error);

    assert_eq!(error.class, crate::agents::TurnErrorClass::PausedRateLimit);
    assert_eq!(
        error.label.as_deref(),
        Some("usage limit inferred (rate-limit window spent)")
    );
}

#[test]
fn codex_turn_death_retry_is_bounded_to_generic_recent_codex_markers() {
    let now = Timestamp::now();
    let marker = crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::Unknown,
        at: now,
        label: Some("turn ended with no final message".to_owned()),
    };

    assert!(codex_turn_death_retry_due("codex", &marker, now));
    assert!(!codex_turn_death_retry_due("claude", &marker, now));

    let classified = crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::Failed,
        ..marker.clone()
    };
    assert!(!codex_turn_death_retry_due("codex", &classified, now));

    let future = crate::agents::AgentTurnError {
        at: now.checked_add(jiff::SignedDuration::from_secs(1)).unwrap(),
        ..marker.clone()
    };
    assert!(!codex_turn_death_retry_due("codex", &future, now));

    let expired = crate::agents::AgentTurnError {
        at: now
            .checked_sub(jiff::SignedDuration::from_secs(
                CODEX_TURN_DEATH_RETRY_WINDOW.as_secs() as i64 + 1,
            ))
            .unwrap(),
        ..marker
    };
    assert!(!codex_turn_death_retry_due("codex", &expired, now));
}

#[test]
fn codex_turn_death_retry_merges_refined_marker() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let at = write_spent_codex_window(&runtime);
    let marker = crate::agents::AgentTurnError {
        class: crate::agents::TurnErrorClass::Unknown,
        at,
        label: Some("turn ended with no final message".to_owned()),
    };
    let mut record = crate::agents::context::record::AgentContextRecord::new(
        "codex",
        "sess-1",
        crate::agents::AgentContext::new("codex", at),
    );
    record.context.turn_error = Some(marker);
    crate::store::agent_context::write_record(&runtime, &record).unwrap();
    let prior = crate::store::agent_context::read_one(&runtime, "codex", "sess-1");

    assert!(
        retry_unconfirmed_codex_turn_death(None, &runtime, "codex", "sess-1", prior.as_ref(),)
            .unwrap()
    );

    let refined = crate::store::agent_context::read_one(&runtime, "codex", "sess-1")
        .unwrap()
        .context
        .turn_error
        .expect("refined turn death remains stamped");
    assert_eq!(refined.at, at);
    assert_eq!(
        refined.class,
        crate::agents::TurnErrorClass::PausedRateLimit
    );
    assert_eq!(
        refined.label.as_deref(),
        Some("usage limit inferred (rate-limit window spent)")
    );
}

fn write_spent_codex_window(runtime: &RuntimePaths) -> Timestamp {
    let now = Timestamp::now();
    let reset = now
        .checked_add(jiff::SignedDuration::from_secs(60 * 60))
        .unwrap();
    let cache = crate::agents::account::RateLimitsCache {
        entries: BTreeMap::from([(
            crate::ids::LoginKey::default_for(crate::ids::AgentKind::new_unchecked("codex")),
            crate::agents::account::RateLimitCacheEntry {
                limits: crate::agents::AgentRateLimits {
                    windows: vec![crate::agents::RateLimitWindow {
                        used_percentage: Some(100),
                        resets_at: Some(reset),
                        duration_mins: Some(300),
                        ..Default::default()
                    }],
                },
                ..Default::default()
            },
        )]),
        ..Default::default()
    };
    std::fs::write(
        runtime.shared_rate_limits_path(),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
    now
}

#[test]
fn unsupported_tick_adapter_writes_no_sidecar() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();

    refresh_session_transcript_context_with_snapshot(
        None,
        &runtime,
        "claude",
        "sess-1",
        Some("opus"),
        RefreshTrigger::Tick,
    );

    assert!(crate::store::agent_context::read_all(&runtime).is_empty());
}

#[test]
fn transcript_backstop_is_stat_gated() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace, dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n\
         {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
         \"last_token_usage\":{\"input_tokens\":50,\"total_tokens\":60},\
         \"model_context_window\":100}}}\n",
    )
    .unwrap();

    let mut record = crate::agents::context::record::AgentContextRecord::new(
        "codex",
        "sess-1",
        crate::agents::AgentContext::new("codex", Timestamp::now()),
    );
    record.transcript_path = Some(path.to_string_lossy().into_owned());
    crate::store::agent_context::write_record(&runtime, &record).unwrap();

    refresh_session_transcript_context_with_snapshot(
        None,
        &runtime,
        "codex",
        "sess-1",
        Some("gpt-5"),
        RefreshTrigger::Tick,
    );
    let first = crate::store::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
    // The sidecar carries the derivation inputs (window + current usage), not a
    // baked percentage; the gauge derives 50% (50 of 100) downstream.
    let first_tokens = first
        .context
        .tokens
        .as_ref()
        .expect("first refresh writes tokens");
    assert_eq!(first_tokens.context_window_size, Some(100));
    assert_eq!(
        first_tokens
            .current_usage
            .as_ref()
            .and_then(|usage| usage.input_tokens),
        Some(50)
    );
    let observed_at = first.context.observed_at;
    let stat = first.transcript_stat;

    refresh_session_transcript_context_with_snapshot(
        None,
        &runtime,
        "codex",
        "sess-1",
        Some("gpt-5"),
        RefreshTrigger::Tick,
    );
    let second = crate::store::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(second.context.observed_at, observed_at);
    assert_eq!(second.transcript_stat, stat);

    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(
            b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
          \"last_token_usage\":{\"input_tokens\":80,\"total_tokens\":90},\
          \"model_context_window\":100}}}\n",
        )
        .unwrap();
    refresh_session_transcript_context_with_snapshot(
        None,
        &runtime,
        "codex",
        "sess-1",
        Some("gpt-5"),
        RefreshTrigger::Tick,
    );
    let third = crate::store::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(
        third
            .context
            .tokens
            .as_ref()
            .and_then(|t| t.current_usage.as_ref())
            .and_then(|usage| usage.input_tokens),
        Some(80)
    );
    assert_ne!(third.transcript_stat, stat);
}

#[test]
fn forced_refresh_bypasses_stat_gate() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-5\"}}\n\
         {\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\
         \"last_token_usage\":{\"input_tokens\":50,\"total_tokens\":60},\
         \"model_context_window\":100}}}\n",
    )
    .unwrap();

    let mut record = crate::agents::context::record::AgentContextRecord::new(
        "codex",
        "sess-1",
        crate::agents::AgentContext::new("codex", Timestamp::now()),
    );
    record.transcript_path = Some(path.to_string_lossy().into_owned());
    crate::store::agent_context::write_record(&runtime, &record).unwrap();

    refresh_session_transcript_context_with_snapshot(
        None,
        &runtime,
        "codex",
        "sess-1",
        Some("gpt-5"),
        RefreshTrigger::Tick,
    );
    let first = crate::store::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
    let observed_at = first.context.observed_at;
    let stat = first.transcript_stat;
    refresh_session_transcript_context_with_snapshot(
        None,
        &runtime,
        "codex",
        "sess-1",
        Some("gpt-5"),
        RefreshTrigger::Tick,
    );
    let second = crate::store::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(second.context.observed_at, observed_at);
    assert_eq!(second.transcript_stat, stat);

    let snapshot = snapshot_with_panels(workspace, Vec::new());
    let mut agent = crate::testkit::agent_state("codex", "sess-1", Timestamp::now());
    agent.model = Some("gpt-5".to_owned());
    let refresh = force_refresh_session_context(&snapshot, &runtime, &agent).unwrap();

    assert!(refresh.transcript_refreshed);
    assert!(refresh.helper_spawned);
    let forced = crate::store::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
    assert_ne!(forced.context.observed_at, observed_at);
    assert_eq!(forced.transcript_stat, stat);
    assert_eq!(
        forced
            .context
            .tokens
            .as_ref()
            .and_then(|tokens| tokens.current_usage.as_ref())
            .and_then(|usage| usage.input_tokens),
        Some(50)
    );
}

#[test]
fn forced_refresh_reruns_turn_death_ladder() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = WorkspaceId::from_project_root(dir.path());
    let runtime = RuntimePaths::under(workspace.clone(), dir.path()).unwrap();
    runtime.ensure_dirs().unwrap();
    let path = dir.path().join("rollout-session.jsonl");
    std::fs::write(
        &path,
        "{\"timestamp\":\"2026-07-03T12:55:00.000Z\",\
         \"type\":\"event_msg\",\
         \"payload\":{\"type\":\"task_complete\",\"last_agent_message\":null}}\n",
    )
    .unwrap();

    let mut record = crate::agents::context::record::AgentContextRecord::new(
        "codex",
        "sess-1",
        crate::agents::AgentContext::new("codex", Timestamp::now()),
    );
    record.transcript_path = Some(path.to_string_lossy().into_owned());
    crate::store::agent_context::write_record(&runtime, &record).unwrap();
    refresh_session_transcript_context_with_snapshot(
        None,
        &runtime,
        "codex",
        "sess-1",
        Some("gpt-5"),
        RefreshTrigger::Tick,
    );
    let stat_gated = crate::store::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
    assert_eq!(
        stat_gated
            .context
            .turn_error
            .as_ref()
            .map(|error| error.class),
        Some(crate::agents::TurnErrorClass::Unknown)
    );

    write_spent_codex_window(&runtime);
    let snapshot = snapshot_with_panels(workspace, Vec::new());
    let mut agent = crate::testkit::agent_state("codex", "sess-1", Timestamp::now());
    agent.model = Some("gpt-5".to_owned());
    let refresh = force_refresh_session_context(&snapshot, &runtime, &agent).unwrap();

    assert!(refresh.transcript_refreshed);
    let forced = crate::store::agent_context::read_one(&runtime, "codex", "sess-1").unwrap();
    let error = forced
        .context
        .turn_error
        .expect("turn death remains stamped");
    assert_eq!(error.class, crate::agents::TurnErrorClass::PausedRateLimit);
    assert_eq!(
        error.label.as_deref(),
        Some("usage limit inferred (rate-limit window spent)")
    );
    assert_eq!(forced.transcript_stat, stat_gated.transcript_stat);
}
