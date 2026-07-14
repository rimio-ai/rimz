use std::collections::BTreeMap;

use jiff::{SignedDuration, Timestamp};
use rimz::agents::RateLimitsCache;
use rimz::agents::context::WindowSource;
use rimz::agents::{AgentAccount, AgentRateLimits, RateLimitWindow};
use rimz::ids::MuxName;
use rimz::sidebar::refresh::{AccountsCache, ProviderRecord};
use rimz::sidebar::timing::unix_now_ms;
use serde_json::json;

use super::{RoomHarness, SETTLE, running_row, session_start_at, user_prompt_submit};
use crate::common::Env;

#[test]
fn rate_limit_recovery_renders_park_and_reset_countdown() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch_wide(&env, MuxName::Tmux);
    room.publish_accounts(&accounts());
    room.publish_rate_limits(&rate_cache(100, future(3600)));

    room.onboard(&["claude"]);
    room.agent_hook(
        "claude",
        &session_start_at(
            "sess-limit",
            "Opus 4.8",
            "high",
            env.project_root.display().to_string(),
            Some("main"),
        ),
    );
    room.agent_hook(
        "claude",
        &user_prompt_submit("sess-limit", "finish release notes"),
    );
    feed_statusline(&room, "sess-limit", 100, future(3600));
    room.agent_hook_in_room_runtime(
        "claude",
        &json!({
            "hook_event_name": "StopFailure",
            "session_id": "sess-limit",
            "error": "rate_limit",
            "last_assistant_message": "You've hit your usage limit"
        }),
    );

    let screen = room.wait_for(
        |s| s.contains("claude") && s.contains('⏸') && s.contains("⏸︎ 1") && s.contains('↻'),
        SETTLE,
    );
    assert!(
        screen.contains("claude") && screen.contains('⏸'),
        "spent rate-limit window should park the row:\n{screen}"
    );
    assert!(
        screen.contains("⏸︎ 1"),
        "cockpit should count the parked row:\n{screen}"
    );
    assert!(
        screen.contains('↻') && (screen.contains('h') || screen.contains('d')),
        "dashboard should render a reset countdown:\n{screen}"
    );

    room.publish_rate_limits(&rate_cache(0, future(5 * 3600)));
    feed_statusline(&room, "sess-limit", 100, past(60));
    let screen = room.wait_for(|s| running_row(s, "claude") && s.contains("⏸︎ 0"), SETTLE);
    assert!(
        running_row(&screen, "claude") && screen.contains("⏸︎ 0"),
        "after the spent window resets the row resumes running and the paused tally clears:\n{screen}"
    );
}

#[test]
fn stalled_stream_error_renders_backoff_park() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    let room = RoomHarness::launch_wide(&env, MuxName::Tmux);

    room.onboard(&["claude"]);
    room.agent_hook(
        "claude",
        &session_start_at(
            "sess-stall",
            "Opus 4.8",
            "high",
            env.project_root.display().to_string(),
            Some("main"),
        ),
    );
    room.agent_hook(
        "claude",
        &user_prompt_submit("sess-stall", "finish release notes"),
    );
    room.agent_hook_in_room_runtime(
        "claude",
        &json!({
            "hook_event_name": "StopFailure",
            "session_id": "sess-stall",
            "error": "overloaded",
            "last_assistant_message": "API Error: Response stalled mid-stream. The response above may be incomplete."
        }),
    );

    let screen = room.wait_for(
        |s| s.contains("claude") && s.contains('⏸') && s.contains("⏸︎ 1"),
        SETTLE,
    );
    assert!(
        screen.contains("claude") && screen.contains('⏸') && screen.contains("⏸︎ 1"),
        "stalled stream should park the row for retry:\n{screen}"
    );
}

fn feed_statusline(room: &RoomHarness<'_>, session_id: &str, used: u8, resets_at: Timestamp) {
    let out = room.run_statusline_feed(
        "claude",
        &json!({
            "session_id": session_id,
            "model": { "id": "claude-opus-4-8", "display_name": "Opus 4.8" },
            "context_window": { "context_window_size": 200000, "used_percentage": 42 },
            "rate_limits": {
                "five_hour": { "used_percentage": used, "resets_at": resets_at.as_second() }
            }
        })
        .to_string(),
    );
    assert!(
        out.status.success(),
        "statusline feed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn accounts() -> AccountsCache {
    let now_ms = unix_now_ms();
    let mut providers: BTreeMap<_, _> = rimz::agents::known_kinds()
        .map(|kind| {
            (
                kind.to_owned(),
                ProviderRecord {
                    probed_at_ms: now_ms,
                    ok: true,
                    account: None,
                },
            )
        })
        .collect();
    providers.insert(
        "claude".to_owned(),
        ProviderRecord {
            probed_at_ms: now_ms,
            ok: true,
            account: Some(AgentAccount {
                scope: Default::default(),
                plan: Some("max".to_owned()),
                account_id: None,
                metered: Some(true),
                version: Some("2.1.158".to_owned()),
                sub_provider: None,
                credentials_updated_at_ms: None,
            }),
        },
    );
    AccountsCache { providers }
}

fn rate_cache(used: u8, resets_at: Timestamp) -> RateLimitsCache {
    RateLimitsCache {
        refreshed_at_ms: unix_now_ms(),
        entries: BTreeMap::from([(
            "claude".to_owned(),
            rimz::agents::RateLimitCacheEntry {
                limits: windows(used, resets_at),
                ..Default::default()
            },
        )]),
        ..Default::default()
    }
}

fn windows(used: u8, resets_at: Timestamp) -> AgentRateLimits {
    AgentRateLimits {
        windows: vec![RateLimitWindow {
            used_percentage: Some(used),
            resets_at: Some(resets_at),
            duration_mins: Some(300),
            observed_at: Some(Timestamp::now()),
            source: WindowSource::Authoritative,
            ..Default::default()
        }],
    }
}

fn future(seconds: i64) -> Timestamp {
    Timestamp::now()
        .checked_add(SignedDuration::from_secs(seconds))
        .expect("future timestamp")
}

fn past(seconds: i64) -> Timestamp {
    Timestamp::now()
        .checked_sub(SignedDuration::from_secs(seconds))
        .expect("past timestamp")
}
