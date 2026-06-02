//! Best-effort provider account probe.
//!
//! Account/plan facts are account-scoped, not session-scoped, and some never
//! ride the session context: Claude's subscription tier comes from `claude auth
//! status`, not its statusline. This module probes those out-of-band facts so
//! the sidebar producer folds a plan label onto the provider dashboard.
//!
//! Producer-only: the Claude probe forks a subprocess, so the elected producer
//! runs it and publishes the result to the shared `accounts.json` cache (TTL'd,
//! single-flighted like the diff stats); consumer tabs read that cache and never
//! fork. The probe here is a pure read — the cross-process memoization lives one
//! layer up, in [`crate::sidebar::snapshot`]'s producer cache.
//!
//! The probe also detects a *logged-in but idle* provider — one with no active
//! session this run — so the dashboard can show your accounts and their budgets
//! whenever you are logged in, not only while an agent is mid-turn. Claude probes
//! via `claude auth status`; Codex reads `~/.codex/auth.json` (a cheap file read,
//! no subprocess). A live session's richer context still wins where both exist.
//!
//! Best-effort by contract: a missing binary, a logged-out account, or
//! unparseable output yields `None`. It never fails a snapshot — account is
//! enrichment, never correctness.

use std::process::{Command, Stdio};

use serde::Deserialize;

use super::AgentAccount;

/// The outcome of an out-of-band account probe. The three arms drive the
/// producer's cache TTL: a `Found` or `LoggedOut` answer is authoritative and
/// rides the long success TTL, while `Unavailable` — a binary that would not run,
/// a non-zero exit, an unreadable file — is a transient failure the producer
/// retries on the short failure TTL instead of pinning the dashboard empty for
/// the full success window.
#[derive(Debug)]
pub enum AccountProbe {
    /// A logged-in account with its plan/metering resolved.
    Found(AgentAccount),
    /// The probe ran and authoritatively found no login (logged out, or an auth
    /// file naming no credential). Cache it like a success: it changes about never.
    LoggedOut,
    /// The probe could not complete — the binary is missing, it exited non-zero,
    /// or its file was unreadable. Retry soon; absence here is not logged-out.
    Unavailable,
}

/// The provider account for `kind`, probed out-of-band. A pure probe — the
/// producer single-flights it behind the `accounts.json` cache, so it forks at
/// most once per refresh, never on the per-tick hot path. An unknown kind has no
/// probe here yet and reads as `LoggedOut` (nothing to retry).
pub fn probe(kind: &str) -> AccountProbe {
    match kind {
        "claude" => probe_claude(),
        "codex" => probe_codex(),
        // Every other provider has no out-of-band login probe here yet.
        _ => AccountProbe::LoggedOut,
    }
}

/// Probe Claude's account via `claude auth status` (JSON on stdout). Captures
/// stdout only — never inherits stdio — so it stays quiet in a TUI. A spawn
/// failure or non-zero exit is `Unavailable` (transient), not a logged-out
/// account, so a missing-then-installed binary recovers on the short retry TTL.
fn probe_claude() -> AccountProbe {
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
    })
}

/// Probe Codex's login from `~/.codex/auth.json` (honoring `CODEX_HOME`). A file
/// read only — never a subprocess. A missing file or a no-login payload is
/// `LoggedOut` (an authoritative answer); only an unexpected IO error — e.g. a
/// permission failure on an existing file — is the transient `Unavailable`.
fn probe_codex() -> AccountProbe {
    let Some(home) = super::codex::app_server::codex_home() else {
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
            metered: Some(false),
        });
    }
    if auth
        .tokens
        .and_then(|tokens| tokens.access_token)
        .is_some_and(|token| !token.is_empty())
    {
        return AccountProbe::Found(AgentAccount {
            plan: None,
            metered: Some(true),
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

    #[test]
    fn parses_a_codex_api_key_login_as_unmetered() {
        let json = br#"{ "OPENAI_API_KEY": "sk-abc", "tokens": null }"#;
        let account = found(parse_codex_auth(json), "api-key login");
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(false));
    }

    #[test]
    fn parses_a_codex_chatgpt_login_as_metered() {
        let json = br#"{
            "OPENAI_API_KEY": null,
            "tokens": { "access_token": "ya29-token", "account_id": "acc_1" }
        }"#;
        let account = found(parse_codex_auth(json), "chatgpt login");
        // The plan tier and budgets ride the live app-server context, not the file.
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(true));
    }

    #[test]
    fn codex_logged_out_or_garbage_is_logged_out() {
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
