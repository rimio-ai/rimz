//! Claude's out-of-band account probe: `claude auth status`.
//!
//! The subscription tier never rides the statusline, so the sidebar producer
//! probes it here ([`AgentAdapter::probe_account`]). Best-effort and
//! producer-only — see [`crate::agents::account`] for the probe contract.
//!
//! [`AgentAdapter::probe_account`]: crate::agents::AgentAdapter::probe_account

use std::process::{Command, Stdio};

use serde::Deserialize;

use crate::agents::account::AccountProbe;
use crate::agents::context::{AgentAccount, RateLimitWindow, WindowSource};
use crate::agents::payload::non_empty_trimmed;

/// Claude's named subscription budget durations.
pub(crate) const FIVE_HOUR_MINS: u32 = 5 * 60;
pub(crate) const SEVEN_DAY_MINS: u32 = 7 * 24 * 60;

/// Normalize one already-parsed Claude subscription window.
pub(crate) fn budget_window(
    utilization: Option<f64>,
    resets_at: Option<jiff::Timestamp>,
    duration_mins: u32,
    source: WindowSource,
) -> Option<RateLimitWindow> {
    let used_percentage = utilization.map(|value| {
        let clamped = value.clamp(0.0, 100.0);
        if clamped > 0.0 && clamped < 100.0 {
            clamped.round().min(99.0) as u8
        } else {
            clamped.round() as u8
        }
    });
    (used_percentage.is_some() || resets_at.is_some()).then_some(RateLimitWindow {
        used_percentage,
        resets_at,
        duration_mins: Some(duration_mins),
        source,
        ..Default::default()
    })
}

/// Probe Claude's account via `claude auth status` (JSON on stdout). Captures
/// stdout only — never inherits stdio — so it stays quiet in a TUI. A spawn
/// failure or non-zero exit is `Unavailable` (transient), not a logged-out
/// account, so a missing-then-installed binary recovers on the short retry TTL.
pub(crate) fn probe() -> AccountProbe {
    let Ok(output) = Command::new("claude")
        .args(["auth", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
    else {
        return AccountProbe::Unavailable;
    };
    if !output.status.success() {
        return AccountProbe::Unavailable;
    }
    let mut probe = parse_claude_auth(&output.stdout);
    if let AccountProbe::Found(account) = &mut probe {
        account.credentials_updated_at_ms = super::oauth_usage::credentials_stamp();
    }
    probe
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeAuthStatus {
    #[serde(default)]
    logged_in: Option<bool>,
    #[serde(default)]
    auth_method: Option<String>,
    #[serde(default)]
    subscription_type: Option<String>,
}

/// Map a `claude auth status` JSON payload onto a probe outcome. The auth method
/// determines metering; the subscription tier is an independent display label.
/// Unparseable output is `Unavailable` (the CLI misbehaved — retry), not a
/// confident logout.
fn parse_claude_auth(stdout: &[u8]) -> AccountProbe {
    let Ok(status) = serde_json::from_slice::<ClaudeAuthStatus>(stdout) else {
        return AccountProbe::Unavailable;
    };
    if status.logged_in == Some(false) {
        return AccountProbe::LoggedOut;
    }
    let auth_method = status.auth_method.as_deref().and_then(non_empty_trimmed);
    let plan = status
        .subscription_type
        .as_deref()
        .and_then(non_empty_trimmed);
    if status.logged_in != Some(true) && plan.is_none() && auth_method.is_none() {
        return AccountProbe::LoggedOut;
    }
    let metered = auth_method.map(|method| method != "apiKey");
    AccountProbe::Found(AgentAccount {
        plan,
        metered,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pull the account out of a `Found` outcome, or fail the test with `label`.
    fn found(probe: AccountProbe, label: &str) -> AgentAccount {
        match probe {
            AccountProbe::Found(account) => account,
            other => panic!("expected {label}, got {other:?}"),
        }
    }

    #[test]
    fn parses_metered_subscription_unmetered_api_key_and_failure_states() {
        let json = br#"{
            "loggedIn": true,
            "authMethod": "claude.ai",
            "apiProvider": "firstParty",
            "email": "user@example.com",
            "subscriptionType": "max"
        }"#;
        let account = found(parse_claude_auth(json), "metered account");
        assert_eq!(account.plan.as_deref(), Some("max"));
        assert_eq!(account.metered, Some(true));

        let json = br#"{ "loggedIn": true, "authMethod": "claude.ai" }"#;
        let account = found(parse_claude_auth(json), "metered account without tier");
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(true));

        let json = br#"{ "loggedIn": true, "authMethod": "apiKey" }"#;
        let account = found(parse_claude_auth(json), "api-key account");
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(false));

        let json = br#"{
            "loggedIn": true,
            "authMethod": " ",
            "subscriptionType": " "
        }"#;
        let account = found(parse_claude_auth(json), "login without method or tier");
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, None);

        let json = br#"{ "loggedIn": true, "subscriptionType": " max " }"#;
        let account = found(parse_claude_auth(json), "tier without method");
        assert_eq!(account.plan.as_deref(), Some("max"));
        assert_eq!(account.metered, None);

        let json = br#"{ "loggedIn": false }"#;
        assert!(matches!(parse_claude_auth(json), AccountProbe::LoggedOut));

        assert!(matches!(
            parse_claude_auth(b"not json"),
            AccountProbe::Unavailable
        ));
    }

    #[test]
    fn budget_window_owns_clamping_omission_and_source() {
        assert!(budget_window(None, None, FIVE_HOUR_MINS, WindowSource::BestEffort).is_none());

        let reset = "2026-07-06T12:00:00Z".parse().unwrap();
        let window = budget_window(
            Some(99.5),
            Some(reset),
            SEVEN_DAY_MINS,
            WindowSource::Authoritative,
        )
        .unwrap();
        assert_eq!(window.used_percentage, Some(99));
        assert_eq!(window.resets_at, Some(reset));
        assert_eq!(window.duration_mins, Some(10_080));
        assert!(window.source.is_authoritative());
        assert_eq!(
            budget_window(Some(100.0), None, FIVE_HOUR_MINS, WindowSource::BestEffort)
                .unwrap()
                .used_percentage,
            Some(100)
        );
    }
}
