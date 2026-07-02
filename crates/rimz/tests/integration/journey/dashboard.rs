use std::collections::BTreeMap;

use jiff::{SignedDuration, Timestamp};
use rimz::agents::AgentAccount;
use rimz::agents::spending::{SpendTally, SpendWindow, Spending};
use rimz::ids::MuxName;
use rimz::sidebar::refresh::AccountsCache;
use rimz::sidebar::timing::unix_now_ms;
use serde_json::json;

use super::{KEY_RIGHT, RoomHarness, SETTLE, session_start_at, user_prompt_submit};
use crate::common::Env;

#[test]
fn provider_dashboard_renders_spend_cost_and_tabs() {
    let env = Env::new();
    if env.skip_if_sandboxed() {
        return;
    }
    write_provider_tab_config(&env);

    let room = RoomHarness::launch_wide(&env, MuxName::Tmux);
    room.publish_accounts(&accounts());
    room.publish_provider_spending(&spending());

    room.onboard(&["claude"]);
    room.agent_hook(
        "claude",
        &session_start_at(
            "sess-dash",
            "Opus 4.8",
            "high",
            env.project_root.display().to_string(),
            Some("main"),
        ),
    );
    room.agent_hook(
        "claude",
        &user_prompt_submit("sess-dash", "ledger refactor"),
    );
    let payload = claude_statusline("sess-dash", 76);
    let out = room.run_statusline_feed("claude", &payload);
    assert!(
        out.status.success(),
        "statusline feed failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let screen = room.wait_for(
        |s| {
            s.contains("Claude Max · v2.1.158")
                && s.contains("$3.50")
                && s.contains("$1.27")
                && s.contains("W:")
                && s.contains("M:")
                && s.contains('▰')
                && s.contains('▱')
                && s.contains('↻')
        },
        SETTLE,
    );
    assert!(
        screen.contains("Claude Max · v2.1.158"),
        "dashboard should render the Claude plan label:\n{screen}"
    );
    assert!(
        screen.contains("$3.50") && screen.contains("$1.27"),
        "dashboard spend and row cost should render:\n{screen}"
    );
    assert!(
        screen.contains("W:") && screen.contains("M:"),
        "dashboard total rows should render:\n{screen}"
    );
    assert!(
        screen.contains('▰') && screen.contains('▱') && screen.contains('↻'),
        "dashboard budget bars should render:\n{screen}"
    );

    room.send_keys(KEY_RIGHT);
    let screen = room.wait_for(|s| s.contains("ChatGPT Pro · v0.139.0"), SETTLE);
    assert!(
        screen.contains("ChatGPT Pro · v0.139.0"),
        "right-arrow should switch the provider tab to Codex:\n{screen}"
    );
}

fn write_provider_tab_config(env: &Env) {
    let dir = env.config_root().join("rimz");
    std::fs::create_dir_all(&dir).expect("mkdir config");
    std::fs::write(
        dir.join("theme.toml"),
        "[theme.display]\nprovider_tabs = \"always\"\nprovider_list = [\"claude\", \"codex\"]\n",
    )
    .expect("write theme config");
}

fn accounts() -> AccountsCache {
    AccountsCache {
        refreshed_at_ms: unix_now_ms(),
        accounts: BTreeMap::from([
            (
                "claude".to_owned(),
                AgentAccount {
                    plan: Some("max".to_owned()),
                    metered: Some(true),
                    version: Some("2.1.158".to_owned()),
                    sub_provider: None,
                },
            ),
            (
                "codex".to_owned(),
                AgentAccount {
                    plan: Some("pro".to_owned()),
                    metered: Some(false),
                    version: Some("0.139.0".to_owned()),
                    sub_provider: None,
                },
            ),
        ]),
        ok: true,
    }
}

fn spending() -> Spending {
    Spending {
        total: spend_tally(4.70, 1.20, 1.20, 15),
        by_provider: BTreeMap::from([
            ("claude".to_owned(), spend_tally(3.50, 0.0, 0.0, 12)),
            ("codex".to_owned(), spend_tally(1.20, 0.0, 0.0, 3)),
        ]),
    }
}

fn spend_tally(headline_usd: f64, week_usd: f64, month_usd: f64, sessions: u32) -> SpendTally {
    SpendTally {
        headline: spend_window(headline_usd, sessions),
        week: spend_window(week_usd, 0),
        month: spend_window(month_usd, 0),
        year: spend_window(headline_usd + week_usd + month_usd, sessions),
    }
}

fn spend_window(usd: f64, sessions: u32) -> SpendWindow {
    SpendWindow {
        usd,
        tokens: 498_000,
        input: 434_000,
        output: 64_000,
        cache_write: 6_000,
        cache_read: 68_000,
        sessions,
    }
}

fn claude_statusline(session_id: &str, used: u8) -> String {
    let now = Timestamp::now();
    let five_hour = now
        .checked_add(SignedDuration::from_secs(3 * 3600))
        .expect("future");
    let seven_day = now
        .checked_add(SignedDuration::from_secs(3 * 86_400))
        .expect("future");
    json!({
        "session_id": session_id,
        "model": { "id": "claude-opus-4-8", "display_name": "Opus 4.8" },
        "version": "2.1.158",
        "context_window": {
            "context_window_size": 200000,
            "used_percentage": 38,
            "current_usage": {
                "input_tokens": 1700,
                "output_tokens": 2300,
                "cache_creation_input_tokens": 6600,
                "cache_read_input_tokens": 68200
            }
        },
        "cost": { "total_cost_usd": 1.27 },
        "rate_limits": {
            "five_hour": { "used_percentage": used, "resets_at": five_hour.as_second() },
            "seven_day": { "used_percentage": 60, "resets_at": seven_day.as_second() }
        }
    })
    .to_string()
}
