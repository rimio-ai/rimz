//! OpenCode account probe.
//!
//! OpenCode's account file maps provider ids to credential types. It exposes
//! no plan tier or rate-limit windows, so the probe reports the provider and
//! whether the selected credential is metered (`oauth`) or unmetered (`api`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::spend::{latest_message_provider, opencode_data_dirs};
use crate::agents::account::AccountProbe;
use crate::agents::context::AgentAccount;

pub(crate) fn probe() -> AccountProbe {
    let Some(path) = auth_path() else {
        return AccountProbe::LoggedOut;
    };
    probe_auth(&path, latest_message_provider())
}

fn auth_path() -> Option<PathBuf> {
    opencode_data_dirs()
        .into_iter()
        .map(|dir| dir.join("auth.json"))
        .find(|path| path.exists())
}

#[derive(Debug, Deserialize)]
struct OpenCodeCredential {
    #[serde(rename = "type")]
    kind: Option<String>,
}

fn probe_auth(path: &Path, used: Option<String>) -> AccountProbe {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return AccountProbe::LoggedOut,
        Err(_) => return AccountProbe::Unavailable,
    };
    let Ok(credentials) = serde_json::from_slice::<BTreeMap<String, OpenCodeCredential>>(&bytes)
    else {
        return AccountProbe::Unavailable;
    };
    let oauth = |kind: &Option<String>| kind.as_deref() == Some("oauth");
    let Some((provider, credential)) = used
        .as_deref()
        .and_then(|used| credentials.get_key_value(used))
        .or_else(|| {
            credentials
                .iter()
                .find(|(_, credential)| oauth(&credential.kind))
        })
        .or_else(|| credentials.iter().next())
    else {
        return AccountProbe::LoggedOut;
    };
    AccountProbe::Found(AgentAccount {
        plan: Some(sub_label(provider, credential.kind.as_deref())),
        metered: match credential.kind.as_deref() {
            Some("oauth") => Some(true),
            Some("api" | "api_key") => Some(false),
            _ => None,
        },
        version: None,
        sub_provider: Some(provider.clone()),
    })
}

fn sub_label(provider: &str, kind: Option<&str>) -> String {
    let name = provider_display(provider);
    match kind {
        Some("oauth") => format!("{name} OAuth"),
        Some("api" | "api_key") => format!("{name} API Key"),
        Some("wellknown") => format!("{name} Wellknown"),
        _ => name,
    }
}

fn provider_display(provider: &str) -> String {
    match provider {
        "anthropic" => "Anthropic".to_owned(),
        "openai" | "openai-codex" => "OpenAI".to_owned(),
        "github-copilot" => "GitHub Copilot".to_owned(),
        "google" | "gemini" => "Google".to_owned(),
        "opencode" => "OpenCode".to_owned(),
        "deepseek" => "DeepSeek".to_owned(),
        other => other.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(probe: AccountProbe, label: &str) -> AgentAccount {
        match probe {
            AccountProbe::Found(account) => account,
            other => panic!("expected {label}, got {other:?}"),
        }
    }

    fn write_auth(dir: &Path, json: &str) -> PathBuf {
        let path = dir.join("auth.json");
        std::fs::write(&path, json).unwrap();
        path
    }

    #[test]
    fn credential_selection_reports_metered_api_and_used_provider() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_auth(
            dir.path(),
            r#"{ "anthropic": { "type": "oauth", "access": "a" } }"#,
        );
        let account = found(probe_auth(&path, None), "oauth account");
        assert_eq!(account.plan.as_deref(), Some("Anthropic OAuth"));
        assert_eq!(account.metered, Some(true));
        assert_eq!(account.sub_provider.as_deref(), Some("anthropic"));

        let path = write_auth(
            dir.path(),
            r#"{
                "anthropic": { "type": "oauth", "access": "a" },
                "openai": { "type": "api", "key": "k" }
            }"#,
        );
        let account = found(
            probe_auth(&path, Some("openai".to_owned())),
            "used provider",
        );
        assert_eq!(account.plan.as_deref(), Some("OpenAI API Key"));
        assert_eq!(account.metered, Some(false));

        let path = write_auth(
            dir.path(),
            r#"{
                "openai": { "type": "api", "key": "k" },
                "opencode": { "type": "wellknown", "key": "z" }
            }"#,
        );
        let account = found(probe_auth(&path, None), "first entry");
        assert_eq!(account.plan.as_deref(), Some("OpenAI API Key"));
        assert_eq!(account.metered, Some(false));
        let account = found(
            probe_auth(&path, Some("opencode".to_owned())),
            "used wellknown provider",
        );
        assert_eq!(account.plan.as_deref(), Some("OpenCode Wellknown"));
        assert_eq!(account.metered, None);
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
}
