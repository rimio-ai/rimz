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

/// The provider account for `kind`, probed out-of-band. `None` when the provider
/// exposes no out-of-band account here (e.g. an unknown kind) or the probe found
/// none. A pure probe — the producer single-flights it behind the `accounts.json`
/// cache, so it forks at most once per refresh, never on the per-tick hot path.
pub fn probe(kind: &str) -> Option<AgentAccount> {
    match kind {
        "claude" => probe_claude(),
        "codex" => probe_codex(),
        // Every other provider has no out-of-band login probe here yet.
        _ => None,
    }
}

/// Probe Claude's account via `claude auth status` (JSON on stdout). Captures
/// stdout only — never inherits stdio — so it stays quiet in a TUI.
fn probe_claude() -> Option<AgentAccount> {
    let output = Command::new("claude")
        .args(["auth", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
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

/// Map a `claude auth status` JSON payload onto an account. A subscription tier
/// marks a metered (rate-limited) account; an API-key login carries no tier and
/// is unmetered — the dashboard's "infinite" bar. `None` when logged out or the
/// payload names neither a tier nor an auth method.
fn parse_claude_auth(stdout: &[u8]) -> Option<AgentAccount> {
    let status: ClaudeAuthStatus = serde_json::from_slice(stdout).ok()?;
    if status.logged_in == Some(false) {
        return None;
    }
    let plan = status.subscription_type.filter(|tier| !tier.is_empty());
    if plan.is_none() && status.auth_method.is_none() {
        return None;
    }
    let metered = plan.is_some() && status.auth_method.as_deref() != Some("apiKey");
    Some(AgentAccount {
        plan,
        metered: Some(metered),
    })
}

/// Probe Codex's login from `~/.codex/auth.json` (honoring `CODEX_HOME`). A file
/// read only — never a subprocess — so it is cheap enough to call for an idle
/// provider on every fold. `None` when the file is absent, unreadable, or names
/// no login.
fn probe_codex() -> Option<AgentAccount> {
    let path = super::codex_app_server::codex_home()?.join("auth.json");
    parse_codex_auth(&std::fs::read(path).ok()?)
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

/// Map a `~/.codex/auth.json` payload onto an account by login shape: a non-empty
/// `OPENAI_API_KEY` is an unmetered API-key login (the dashboard's `∞` bar), and
/// a `tokens` block is a metered ChatGPT subscription login. The plan tier and
/// budget windows ride the live app-server context, so an idle login carries no
/// plan label here. `None` when neither login is present (logged out).
fn parse_codex_auth(auth_json: &[u8]) -> Option<AgentAccount> {
    let auth: CodexAuth = serde_json::from_slice(auth_json).ok()?;
    if auth
        .openai_api_key
        .as_deref()
        .is_some_and(|key| !key.is_empty())
    {
        return Some(AgentAccount {
            plan: None,
            metered: Some(false),
        });
    }
    if auth
        .tokens
        .and_then(|tokens| tokens.access_token)
        .is_some_and(|token| !token.is_empty())
    {
        return Some(AgentAccount {
            plan: None,
            metered: Some(true),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_subscription_account_as_metered() {
        let json = br#"{
            "loggedIn": true,
            "authMethod": "claude.ai",
            "apiProvider": "firstParty",
            "email": "rimio.ai@gmail.com",
            "subscriptionType": "max"
        }"#;
        let account = parse_claude_auth(json).expect("metered account");
        assert_eq!(account.plan.as_deref(), Some("max"));
        assert_eq!(account.metered, Some(true));
    }

    #[test]
    fn parses_an_api_key_account_as_unmetered() {
        let json = br#"{ "loggedIn": true, "authMethod": "apiKey" }"#;
        let account = parse_claude_auth(json).expect("api-key account");
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(false));
    }

    #[test]
    fn logged_out_is_no_account() {
        let json = br#"{ "loggedIn": false }"#;
        assert!(parse_claude_auth(json).is_none());
    }

    #[test]
    fn garbage_output_is_no_account() {
        assert!(parse_claude_auth(b"not json").is_none());
    }

    #[test]
    fn parses_a_codex_api_key_login_as_unmetered() {
        let json = br#"{ "OPENAI_API_KEY": "sk-abc", "tokens": null }"#;
        let account = parse_codex_auth(json).expect("api-key login");
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(false));
    }

    #[test]
    fn parses_a_codex_chatgpt_login_as_metered() {
        let json = br#"{
            "OPENAI_API_KEY": null,
            "tokens": { "access_token": "ya29-token", "account_id": "acc_1" }
        }"#;
        let account = parse_codex_auth(json).expect("chatgpt login");
        // The plan tier and budgets ride the live app-server context, not the file.
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(true));
    }

    #[test]
    fn codex_logged_out_or_garbage_is_no_account() {
        assert!(parse_codex_auth(br#"{ "OPENAI_API_KEY": null }"#).is_none());
        assert!(parse_codex_auth(br#"{ "tokens": { "access_token": "" } }"#).is_none());
        assert!(parse_codex_auth(b"not json").is_none());
    }
}
