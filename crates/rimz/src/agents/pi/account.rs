//! Pi's out-of-band account probe: the `auth.json` credential map under Pi's
//! config root.
//!
//! Pi exposes no plan tier in its account file; provider windows arrive through
//! the extension's response-header capture and the OAuth usage probe. The probe
//! labels the subscription the fleet actually uses: the provider of the
//! freshest session, tail-read from the newest session JSONL, falling back to
//! the first OAuth credential, else the first entry. The binary version is
//! separate display enrichment exposed through
//! [`crate::agents::AgentAdapter::probe_version`], so an active Pi session can
//! show `pi --version` even when no account file exists. Best-effort and
//! producer-only — see [`crate::agents::account`] for the probe contract.
//!
//! [Pi protocol reference]: ../../../../../docs/externals/agent-adapter/pi-reference.md

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

use super::spend::{pi_config_dir, pi_session_files};
use crate::agents::account::AccountProbe;
use crate::agents::context::{AgentAccount, ProviderAccountScope};
use crate::agents::read_transcript_tail;

/// Probe Pi's account: parse the auth file and label the used subscription.
/// The missing-file fast path skips the session walk — the common Pi-less
/// machine pays one `stat`; `probe_auth` re-handles a racing removal on its own
/// read.
pub(crate) fn probe() -> AccountProbe {
    let path = pi_config_dir().join("auth.json");
    if !path.exists() {
        return AccountProbe::LoggedOut;
    }
    probe_auth(&path, used_provider())
}

/// One auth.json credential: `{ "type": "oauth" | "api_key", … }`. The token
/// and key fields are deliberately not modeled — the probe needs the
/// credential *type*, never the secret.
#[derive(Debug, Deserialize)]
struct PiCredential {
    #[serde(rename = "type")]
    kind: Option<String>,
}

/// Map the auth file onto a probe outcome. A missing file, or one naming no
/// credential, is an authoritative `LoggedOut`; an unreadable or unparseable
/// file is `Unavailable` (transient — retry on the short TTL). The `used`
/// provider picks the labeled credential when it holds one; otherwise the
/// first OAuth entry leads (a subscription outranks a key for "the sub it
/// used"), else the first entry by name.
fn probe_auth(path: &Path, used: Option<String>) -> AccountProbe {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return AccountProbe::LoggedOut,
        Err(_) => return AccountProbe::Unavailable,
    };
    let Ok(credentials) = serde_json::from_slice::<BTreeMap<String, PiCredential>>(&bytes) else {
        return AccountProbe::Unavailable;
    };
    let oauth = |kind: &Option<String>| kind.as_deref() == Some("oauth");
    let Some((provider, credential)) = used
        .as_deref()
        .and_then(|used| credentials.get_key_value(used))
        .or_else(|| credentials.iter().find(|(_, cred)| oauth(&cred.kind)))
        .or_else(|| credentials.iter().next())
    else {
        return AccountProbe::LoggedOut;
    };
    let scope = (credential.kind.as_deref() == Some("oauth"))
        .then(|| oauth_scope(provider))
        .flatten()
        .unwrap_or_default();
    AccountProbe::Found(AgentAccount {
        scope,
        plan: Some(sub_label(provider, credential.kind.as_deref())),
        account_id: None,
        // The reference mapping: an OAuth credential is a metered
        // subscription, an API key is unmetered, and an unknown type stays
        // unknown. Pi's own window feeders publish under the `pi` kind.
        metered: match credential.kind.as_deref() {
            Some("oauth") => Some(true),
            Some("api_key") => Some(false),
            _ => None,
        },
        version: None,
        // The raw credential key (`anthropic`, `openai`, …), retained so the
        // panel can name which backing subscription Pi is using.
        sub_provider: Some(provider.clone()),
        credentials_updated_at_ms: crate::agents::account::file_mtime_ms(path),
    })
}

/// Stable quota-cache scope for OAuth providers Pi can query directly.
pub(super) fn oauth_scope(provider: &str) -> Option<ProviderAccountScope> {
    match provider {
        "openai" | "openai-codex" => Some(ProviderAccountScope::sub_provider("openai", "oauth")),
        "anthropic" => Some(ProviderAccountScope::sub_provider("anthropic", "oauth")),
        _ => None,
    }
}

/// The raw plan string for a credential: the provider's display name plus the
/// credential type (`Anthropic OAuth`, `OpenAI API Key`). Emitted with its
/// casing already in place — the renderer's title-casing only touches word
/// initials, so `OAuth`, `Key`, and `GitHub` pass through it unchanged and the
/// cached label equals the rendered one.
fn sub_label(provider: &str, kind: Option<&str>) -> String {
    let name = provider_display(provider);
    match kind {
        Some("oauth") => format!("{name} OAuth"),
        Some("api_key") => format!("{name} API Key"),
        _ => name,
    }
}

/// Brand-cased display names for the providers Pi ships OAuth flows for; an
/// unknown provider id passes through raw and earns the renderer's generic
/// title-casing.
fn provider_display(provider: &str) -> String {
    match provider {
        "anthropic" => "Anthropic".to_owned(),
        "openai" | "openai-codex" => "OpenAI".to_owned(),
        "github-copilot" => "GitHub Copilot".to_owned(),
        "google" | "gemini" => "Google".to_owned(),
        other => other.to_owned(),
    }
}

/// The provider of the freshest Pi session — "the sub it used". The newest
/// session file by mtime, tail-scanned newest-first for the last message's
/// `message.provider`. Best-effort: any miss yields `None` and the credential
/// map decides alone.
pub(super) fn used_provider() -> Option<String> {
    let (_, newest) = pi_session_files()
        .into_iter()
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).ok()?.modified().ok()?;
            Some((modified, path))
        })
        .max_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)))?;
    let tail = read_transcript_tail(&newest)?;
    tail.lines().rev().find_map(provider_of_line)
}

#[derive(Deserialize)]
struct ProviderEntry {
    message: Option<ProviderMessage>,
}

#[derive(Deserialize)]
struct ProviderMessage {
    provider: Option<String>,
}

fn provider_of_line(line: &str) -> Option<String> {
    if !line.contains(r#""provider""#) {
        return None;
    }
    serde_json::from_str::<ProviderEntry>(line)
        .ok()?
        .message?
        .provider
        .filter(|provider| !provider.is_empty())
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

    fn write_auth(dir: &Path, json: &str) -> std::path::PathBuf {
        let path = dir.join("auth.json");
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn credential_selection_reports_metered_api_key_and_session_provider_cases() {
        let dir = tempfile::tempdir().unwrap();

        let path = write_auth(
            dir.path(),
            r#"{ "anthropic": { "type": "oauth", "access": "a", "refresh": "r", "expires": 1 } }"#,
        );
        let account = found(probe_auth(&path, None), "oauth account");
        assert_eq!(account.plan.as_deref(), Some("Anthropic OAuth"));
        assert_eq!(account.metered, Some(true));
        assert_eq!(
            account.scope,
            ProviderAccountScope::sub_provider("anthropic", "oauth")
        );
        assert_eq!(account.version, None);
        // The raw credential key rides along, so the dashboard can name the
        // backing subscription Pi is using.
        assert_eq!(account.sub_provider.as_deref(), Some("anthropic"));
        assert!(account.credentials_updated_at_ms.is_some());

        let path = write_auth(
            dir.path(),
            r#"{
                "anthropic": { "type": "oauth", "access": "a" },
                "openai": { "type": "api_key", "key": "k" }
            }"#,
        );
        let account = found(
            probe_auth(&path, Some("openai".to_owned())),
            "used provider",
        );
        assert_eq!(account.plan.as_deref(), Some("OpenAI API Key"));
        assert_eq!(account.metered, Some(false));
        assert_eq!(account.scope, ProviderAccountScope::KindWide);

        let path = write_auth(
            dir.path(),
            r#"{
                "openai": { "type": "api_key", "key": "k" },
                "openai-codex": { "type": "oauth", "access": "a" }
            }"#,
        );
        let account = found(probe_auth(&path, None), "oauth lead");
        assert_eq!(account.plan.as_deref(), Some("OpenAI OAuth"));
        assert_eq!(account.metered, Some(true));
        assert_eq!(
            account.scope,
            ProviderAccountScope::sub_provider("openai", "oauth")
        );
    }

    #[test]
    fn missing_empty_and_garbage_auth_states_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            probe_auth(&dir.path().join("auth.json"), None),
            AccountProbe::LoggedOut
        ));

        let path = write_auth(dir.path(), "{}");
        assert!(matches!(probe_auth(&path, None), AccountProbe::LoggedOut));

        let path = write_auth(dir.path(), "not json");
        assert!(matches!(probe_auth(&path, None), AccountProbe::Unavailable));
    }

    #[test]
    fn provider_line_reads_the_assistant_message_provider() {
        let line = r#"{"type":"message","id":"a1","message":{"role":"assistant","provider":"openai-codex","model":"gpt-5.5","usage":{"input":1}}}"#;
        assert_eq!(provider_of_line(line).as_deref(), Some("openai-codex"));
        assert_eq!(provider_of_line(r#"{"type":"session","version":3}"#), None);
        assert_eq!(
            provider_of_line(r#"{"message":{"role":"user","content":[]}}"#),
            None
        );
    }
}
