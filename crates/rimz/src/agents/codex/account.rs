//! Codex's out-of-band account probe: `auth.json`, then `codex login status`.
//!
//! The file path keeps the common case cheap and distinguishes a metered
//! ChatGPT subscription from an unmetered API key. Codex can instead keep
//! credentials in the OS keyring; when no file exists, the CLI status command
//! supplies the same distinction. Best-effort and producer-only — see
//! [`crate::agents::account`] for the probe contract.
//!
//! [`AgentAdapter::probe_account`]: crate::agents::AgentAdapter::probe_account

use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::Value;

use crate::agents::account::AccountProbe;
use crate::agents::context::{
    AgentAccount, AgentRateLimits, RateLimitWindow, RateLimitWindowScope, WindowSource,
};
use crate::agents::payload::non_empty_trimmed;
use crate::agents::{AccountUsageSnapshot, ExtraCredits};

const FIVE_HOUR_MINS: u32 = 5 * 60;

/// Transport-neutral input for one Codex/OpenAI rate-limit row.
#[derive(Debug)]
pub(crate) struct UsageWindow {
    pub(crate) used_percentage: Option<f64>,
    pub(crate) resets_at: Option<jiff::Timestamp>,
    pub(crate) duration_mins: Option<u32>,
    pub(crate) scope: Option<RateLimitWindowScope>,
    pub(crate) source: WindowSource,
}

/// Transport-neutral input for Codex paid credits.
#[derive(Debug)]
pub(crate) struct UsageCredits {
    pub(crate) has_credits: Option<bool>,
    pub(crate) unlimited: Option<bool>,
    pub(crate) overage_limit_reached: Option<bool>,
    pub(crate) balance: Option<f64>,
}

/// Normalize Codex/OpenAI account usage after transport-specific parsing.
pub(crate) fn normalize_usage(
    plan: Option<String>,
    windows: impl IntoIterator<Item = UsageWindow>,
    credits: Option<UsageCredits>,
) -> AccountUsageSnapshot {
    let mut windows: Vec<RateLimitWindow> = windows
        .into_iter()
        .map(|window| RateLimitWindow {
            used_percentage: window
                .used_percentage
                .map(|value| value.round().clamp(0.0, 100.0) as u8),
            resets_at: window.resets_at,
            duration_mins: window.duration_mins,
            scope: window.scope,
            source: window.source,
            ..Default::default()
        })
        .filter(|window| {
            window.used_percentage.is_some()
                || window.resets_at.is_some()
                || window.duration_mins.is_some()
                || window.scope.is_some()
        })
        .collect();
    let completes_five_hour = windows.iter().any(|window| {
        window.scope.is_none() && window.duration_mins.is_some() && window.source.is_authoritative()
    });
    if completes_five_hour
        && !windows
            .iter()
            .any(|window| window.scope.is_none() && window.duration_mins == Some(FIVE_HOUR_MINS))
    {
        windows.push(RateLimitWindow {
            duration_mins: Some(FIVE_HOUR_MINS),
            source: WindowSource::Authoritative,
            lifted: true,
            ..Default::default()
        });
    }
    windows.sort_by_key(|window| window.duration_mins.unwrap_or(u32::MAX));
    AccountUsageSnapshot {
        rate_limits: (!windows.is_empty()).then_some(AgentRateLimits { windows }),
        extra_credits: credits.and_then(normalize_credits),
        plan: plan.as_deref().and_then(non_empty_trimmed),
        ..Default::default()
    }
}

pub(crate) fn normalize_credits(credits: UsageCredits) -> Option<ExtraCredits> {
    if credits.overage_limit_reached == Some(true) {
        return Some(ExtraCredits::known(None, Some(0.0), None));
    }
    if credits.unlimited == Some(true) {
        return Some(ExtraCredits::known(None, None, None));
    }
    if let Some(balance) = credits.balance {
        return Some(ExtraCredits::known(None, Some(balance), None));
    }
    (credits.has_credits == Some(false)).then_some(ExtraCredits::Disabled)
}

pub(crate) fn parse_balance(value: &Value) -> Option<f64> {
    match value {
        Value::Number(value) => value.as_f64().filter(|value| value.is_finite()),
        Value::String(value) => value.trim().parse::<f64>().ok(),
        _ => None,
    }
    .filter(|value| value.is_finite())
    .map(|value| value.max(0.0))
}

/// Probe Codex's login from `~/.codex/auth.json` (honoring `CODEX_HOME`), then
/// fall back to `codex login status` when credentials live in the OS keyring.
/// A readable file remains authoritative; an unexpected file IO error is the
/// transient `Unavailable` arm.
pub(crate) fn probe() -> AccountProbe {
    let Some(home) = super::app_server::codex_home() else {
        return AccountProbe::LoggedOut;
    };
    let path = home.join("auth.json");
    match std::fs::read(&path) {
        Ok(bytes) => with_credentials_mtime(parse_codex_auth(&bytes), &path),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => probe_login_status(),
        Err(_) => AccountProbe::Unavailable,
    }
}

fn with_credentials_mtime(probe: AccountProbe, path: &std::path::Path) -> AccountProbe {
    match probe {
        AccountProbe::Found(mut account) => {
            account.credentials_updated_at_ms = crate::agents::account::file_mtime_ms(path);
            AccountProbe::Found(account)
        }
        other => other,
    }
}

fn probe_login_status() -> AccountProbe {
    let Ok(output) = Command::new("codex")
        .args(["login", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
    else {
        return AccountProbe::Unavailable;
    };
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    parse_login_status(&text)
}

/// Map `codex login status` output onto a probe outcome. Codex prints one line
/// per auth mode (`run_login_status`): `Logged in using ChatGPT` (a metered
/// subscription), `Logged in using an API key - <masked>` and `Logged in using
/// Amazon Bedrock API key` (token/AWS-billed, so unmetered by subscription
/// windows), `Logged in using access token` / `Logged in using personal access
/// token` (logged in, metering unknown — the dashboard infers it from window
/// presence), or `Not logged in`. Any other recognized `Logged in using …` mode
/// is authoritatively logged in with unknown metering rather than a retried
/// `Unavailable`; only output with no recognizable login line is transient.
fn parse_login_status(text: &str) -> AccountProbe {
    let normalized = text.trim().to_ascii_lowercase();
    let mut metered = None;
    for line in normalized.lines() {
        let line = line.trim();
        if line == "not logged in" {
            return AccountProbe::LoggedOut;
        }
        let Some(mode) = line.strip_prefix("logged in using ") else {
            continue;
        };
        metered = Some(if mode == "chatgpt" {
            Some(true)
        } else if mode.starts_with("an api key") || mode.starts_with("amazon bedrock api key") {
            Some(false)
        } else {
            None
        });
    }
    let Some(metered) = metered else {
        return AccountProbe::Unavailable;
    };
    AccountProbe::Found(AgentAccount {
        metered,
        ..Default::default()
    })
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct CodexAuth {
    #[serde(default, rename = "OPENAI_API_KEY")]
    pub(super) openai_api_key: Option<String>,
    #[serde(default)]
    pub(super) tokens: Option<CodexTokens>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct CodexTokens {
    #[serde(default)]
    pub(super) access_token: Option<String>,
    #[serde(default)]
    pub(super) account_id: Option<String>,
}

pub(super) fn decode_auth(auth_json: &[u8]) -> serde_json::Result<CodexAuth> {
    serde_json::from_slice(auth_json)
}

/// Map a `~/.codex/auth.json` payload onto a probe outcome by login shape: a
/// non-empty `OPENAI_API_KEY` is an unmetered API-key login (the dashboard's `∞`
/// bar), and a `tokens` block is a metered ChatGPT subscription login. The plan
/// tier and budget windows ride the live app-server context, so an idle login
/// carries no plan label here. `LoggedOut` when neither login is present, or the
/// file is unparseable — a corrupt auth file is rewritten on the next login, and
/// the read is cheap, so there is nothing a short retry would recover.
fn parse_codex_auth(auth_json: &[u8]) -> AccountProbe {
    let Ok(auth) = decode_auth(auth_json) else {
        return AccountProbe::LoggedOut;
    };
    if auth
        .openai_api_key
        .as_deref()
        .is_some_and(|key| !key.is_empty())
    {
        return AccountProbe::Found(AgentAccount {
            metered: Some(false),
            ..Default::default()
        });
    }
    if let Some(tokens) = auth.tokens
        && tokens.access_token.is_some_and(|token| !token.is_empty())
    {
        return AccountProbe::Found(AgentAccount {
            account_id: tokens.account_id.as_deref().and_then(non_empty_trimmed),
            metered: Some(true),
            ..Default::default()
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
            "OPENAI_API_KEY": "sk-abc",
            "tokens": { "access_token": "ya29-token", "account_id": "acc_1" }
        }"#;
        let account = found(parse_codex_auth(json), "mixed API-key and ChatGPT login");
        assert_eq!(account.account_id, None);
        assert_eq!(account.metered, Some(false));

        let json = br#"{
            "OPENAI_API_KEY": null,
            "tokens": { "access_token": "ya29-token", "account_id": "acc_1" }
        }"#;
        let account = found(parse_codex_auth(json), "chatgpt login");
        // The plan tier and budgets ride the live app-server context, not the file.
        assert_eq!(account.plan, None);
        assert_eq!(account.account_id.as_deref(), Some("acc_1"));
        assert_eq!(account.metered, Some(true));

        // A readable auth file remains authoritative. The CLI fallback is only
        // for a missing file, which is the keyring-backed credential shape.
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

    #[test]
    fn file_probe_carries_credential_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let json = br#"{ "OPENAI_API_KEY": "sk-abc" }"#;
        std::fs::write(&path, json).unwrap();
        let account = found(
            with_credentials_mtime(parse_codex_auth(json), &path),
            "file login",
        );
        assert!(account.credentials_updated_at_ms.is_some());
    }

    #[test]
    fn parses_cli_login_status_for_keyring_credentials() {
        // Strings mirror Codex `run_login_status` output verbatim (0.144.1).
        let account = found(
            parse_login_status("Logged in using ChatGPT\n"),
            "keyring ChatGPT login",
        );
        assert_eq!(account.metered, Some(true));

        let account = found(
            parse_login_status("Logged in using an API key - sk-proj-***abcd\n"),
            "keyring API login",
        );
        assert_eq!(account.metered, Some(false));

        let account = found(
            parse_login_status("Logged in using Amazon Bedrock API key\n"),
            "keyring Bedrock API login",
        );
        assert_eq!(account.metered, Some(false));

        // `AuthMode::AgentIdentity` prints "access token", not "agent identity".
        let account = found(
            parse_login_status("Logged in using access token\n"),
            "managed agent-identity login",
        );
        assert_eq!(account.metered, None);

        let account = found(
            parse_login_status("Logged in using personal access token\n"),
            "personal access-token login",
        );
        assert_eq!(account.metered, None);

        assert!(matches!(
            parse_login_status("Not logged in\n"),
            AccountProbe::LoggedOut
        ));
        assert!(matches!(
            parse_login_status("Error checking login status: boom\n"),
            AccountProbe::Unavailable
        ));
    }

    #[test]
    fn usage_normalization_orders_clamps_credits_and_lifts_missing_five_hour() {
        let usage = normalize_usage(
            Some(" pro ".to_owned()),
            [UsageWindow {
                used_percentage: Some(120.0),
                resets_at: None,
                duration_mins: Some(43_800),
                scope: None,
                source: WindowSource::Authoritative,
            }],
            Some(UsageCredits {
                has_credits: Some(false),
                unlimited: None,
                overage_limit_reached: None,
                balance: Some(12.5),
            }),
        );
        assert_eq!(usage.plan.as_deref(), Some("pro"));
        let windows = usage.rate_limits.unwrap().windows;
        assert_eq!(windows[0].duration_mins, Some(300));
        assert!(windows[0].lifted);
        assert_eq!(windows[1].duration_mins, Some(43_800));
        assert_eq!(windows[1].used_percentage, Some(100));
        assert_eq!(
            usage.extra_credits,
            Some(ExtraCredits::known(None, Some(12.5), None))
        );
    }

    #[test]
    fn real_five_hour_and_non_temporal_readings_are_not_lifted() {
        let real = normalize_usage(
            None,
            [UsageWindow {
                used_percentage: Some(42.0),
                resets_at: None,
                duration_mins: Some(300),
                scope: None,
                source: WindowSource::Authoritative,
            }],
            None,
        );
        assert_eq!(real.rate_limits.unwrap().windows.len(), 1);

        for window in [
            UsageWindow {
                used_percentage: Some(42.0),
                resets_at: None,
                duration_mins: Some(10_080),
                scope: None,
                source: WindowSource::BestEffort,
            },
            UsageWindow {
                used_percentage: Some(42.0),
                resets_at: None,
                duration_mins: None,
                scope: Some(RateLimitWindowScope {
                    id: "named".to_owned(),
                    label: "Named".to_owned(),
                }),
                source: WindowSource::Authoritative,
            },
        ] {
            let usage = normalize_usage(None, [window], None);
            assert!(
                usage
                    .rate_limits
                    .unwrap()
                    .windows
                    .iter()
                    .all(|row| !row.lifted)
            );
        }
    }
}
