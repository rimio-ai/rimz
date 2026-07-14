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
use crate::agents::context::AgentAccount;

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
    parse_claude_auth(&output.stdout)
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
    let auth_method = status.auth_method.and_then(non_empty_trimmed);
    let plan = status.subscription_type.and_then(non_empty_trimmed);
    if status.logged_in != Some(true) && plan.is_none() && auth_method.is_none() {
        return AccountProbe::LoggedOut;
    }
    let metered = auth_method.map(|method| method != "apiKey");
    AccountProbe::Found(AgentAccount {
        scope: Default::default(),
        plan,
        account_id: None,
        metered,
        version: None,
        sub_provider: None,
        credentials_updated_at_ms: None,
    })
}

fn non_empty_trimmed(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
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
}
