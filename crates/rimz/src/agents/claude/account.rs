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

/// Map a `claude auth status` JSON payload onto a probe outcome. A subscription
/// tier marks a metered (rate-limited) account; an API-key login carries no tier
/// and is unmetered — the dashboard's "infinite" bar. A logged-out, or a valid
/// payload naming neither a tier nor an auth method, is `LoggedOut`; unparseable
/// output is `Unavailable` (the CLI misbehaved — retry), not a confident logout.
fn parse_claude_auth(stdout: &[u8]) -> AccountProbe {
    let Ok(status) = serde_json::from_slice::<ClaudeAuthStatus>(stdout) else {
        return AccountProbe::Unavailable;
    };
    if status.logged_in == Some(false) {
        return AccountProbe::LoggedOut;
    }
    let plan = status.subscription_type.filter(|tier| !tier.is_empty());
    if plan.is_none() && status.auth_method.is_none() {
        return AccountProbe::LoggedOut;
    }
    let metered = plan.is_some() && status.auth_method.as_deref() != Some("apiKey");
    AccountProbe::Found(AgentAccount {
        plan,
        metered: Some(metered),
        version: None,
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
    fn parses_a_subscription_account_as_metered() {
        let json = br#"{
            "loggedIn": true,
            "authMethod": "claude.ai",
            "apiProvider": "firstParty",
            "email": "rimio.ai@gmail.com",
            "subscriptionType": "max"
        }"#;
        let account = found(parse_claude_auth(json), "metered account");
        assert_eq!(account.plan.as_deref(), Some("max"));
        assert_eq!(account.metered, Some(true));
    }

    #[test]
    fn parses_an_api_key_account_as_unmetered() {
        let json = br#"{ "loggedIn": true, "authMethod": "apiKey" }"#;
        let account = found(parse_claude_auth(json), "api-key account");
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(false));
    }

    #[test]
    fn logged_out_is_logged_out_not_a_failure() {
        let json = br#"{ "loggedIn": false }"#;
        assert!(matches!(parse_claude_auth(json), AccountProbe::LoggedOut));
    }

    #[test]
    fn garbage_output_is_unavailable_so_it_retries_soon() {
        assert!(matches!(
            parse_claude_auth(b"not json"),
            AccountProbe::Unavailable
        ));
    }
}
