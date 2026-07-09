//! Codex's out-of-band account probe: a `~/.codex/auth.json` read.
//!
//! A cheap file read, never a subprocess — the login shape alone separates a
//! metered ChatGPT subscription from an unmetered API key
//! ([`AgentAdapter::probe_account`]). Best-effort and producer-only — see
//! [`crate::agents::account`] for the probe contract.
//!
//! [`AgentAdapter::probe_account`]: crate::agents::AgentAdapter::probe_account

use serde::Deserialize;

use crate::agents::account::AccountProbe;
use crate::agents::context::AgentAccount;

/// Probe Codex's login from `~/.codex/auth.json` (honoring `CODEX_HOME`). A file
/// read only — never a subprocess. A missing file or a no-login payload is
/// `LoggedOut` (an authoritative answer); only an unexpected IO error — e.g. a
/// permission failure on an existing file — is the transient `Unavailable`.
pub(crate) fn probe() -> AccountProbe {
    let Some(home) = super::app_server::codex_home() else {
        return AccountProbe::LoggedOut;
    };
    match std::fs::read(home.join("auth.json")) {
        Ok(bytes) => parse_codex_auth(&bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => AccountProbe::LoggedOut,
        Err(_) => AccountProbe::Unavailable,
    }
}

#[derive(Debug, Default, Deserialize)]
struct CodexAuth {
    #[serde(default, rename = "OPENAI_API_KEY")]
    openai_api_key: Option<String>,
    #[serde(default)]
    tokens: Option<CodexTokens>,
}

#[derive(Debug, Default, Deserialize)]
struct CodexTokens {
    #[serde(default)]
    access_token: Option<String>,
}

/// Map a `~/.codex/auth.json` payload onto a probe outcome by login shape: a
/// non-empty `OPENAI_API_KEY` is an unmetered API-key login (the dashboard's `∞`
/// bar), and a `tokens` block is a metered ChatGPT subscription login. The plan
/// tier and budget windows ride the live app-server context, so an idle login
/// carries no plan label here. `LoggedOut` when neither login is present, or the
/// file is unparseable — a corrupt auth file is rewritten on the next login, and
/// the read is cheap, so there is nothing a short retry would recover.
fn parse_codex_auth(auth_json: &[u8]) -> AccountProbe {
    let Ok(auth) = serde_json::from_slice::<CodexAuth>(auth_json) else {
        return AccountProbe::LoggedOut;
    };
    if auth
        .openai_api_key
        .as_deref()
        .is_some_and(|key| !key.is_empty())
    {
        return AccountProbe::Found(AgentAccount {
            plan: None,
            account_id: None,
            metered: Some(false),
            version: None,
            sub_provider: None,
        });
    }
    if auth
        .tokens
        .and_then(|tokens| tokens.access_token)
        .is_some_and(|token| !token.is_empty())
    {
        return AccountProbe::Found(AgentAccount {
            plan: None,
            account_id: None,
            metered: Some(true),
            version: None,
            sub_provider: None,
        });
    }
    AccountProbe::LoggedOut
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
    fn parses_codex_auth_metering_and_logged_out_states() {
        let json = br#"{ "OPENAI_API_KEY": "sk-abc", "tokens": null }"#;
        let account = found(parse_codex_auth(json), "api-key login");
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(false));

        let json = br#"{
            "OPENAI_API_KEY": null,
            "tokens": { "access_token": "ya29-token", "account_id": "acc_1" }
        }"#;
        let account = found(parse_codex_auth(json), "chatgpt login");
        // The plan tier and budgets ride the live app-server context, not the file.
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(true));

        // A codex auth file read is cheap and never a subprocess, so an absent
        // credential — or an unparseable file — is an authoritative logged-out,
        // not the transient `Unavailable` that drives a short retry.
        assert!(matches!(
            parse_codex_auth(br#"{ "OPENAI_API_KEY": null }"#),
            AccountProbe::LoggedOut
        ));
        assert!(matches!(
            parse_codex_auth(br#"{ "tokens": { "access_token": "" } }"#),
            AccountProbe::LoggedOut
        ));
        assert!(matches!(
            parse_codex_auth(b"not json"),
            AccountProbe::LoggedOut
        ));
    }
}
