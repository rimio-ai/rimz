//! OpenCode OAuth account-usage probe.
//!
//! OpenCode's `auth.json` stores provider OAuth tokens for the backing
//! subscriptions a session runs on. OpenCode itself exposes no quota surface, so
//! this is the agent's only account-usage channel: select the active provider
//! credential and delegate the fetch to the sibling provider's usage probe (the
//! same Anthropic/ChatGPT endpoints Claude and Codex already parse). Read-only —
//! it never refreshes or writes auth files.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::agents::AccountUsageSnapshot;
use crate::agents::credits::{OauthReportable, file_mtime_ms};

use super::spend::{latest_message_provider, opencode_data_dirs};

#[derive(Debug, thiserror::Error)]
pub(crate) enum OpencodeOauthUsageErr {
    #[error("opencode OAuth credentials not found")]
    NoCredentials,
    #[error("opencode auth file selected an API-key credential")]
    ApiKeyOnly,
    #[error("opencode OAuth token is expired")]
    TokenExpired,
    #[error("opencode OAuth usage is unsupported for provider `{0}`")]
    UnsupportedProvider(String),
    #[error("reading opencode OAuth credentials: {0}")]
    Io(#[from] std::io::Error),
    #[error("parsing opencode OAuth credentials: {0}")]
    Parse(#[from] serde_json::Error),
    #[error(transparent)]
    Claude(#[from] crate::agents::claude::oauth_usage::ClaudeOauthUsageErr),
    #[error(transparent)]
    Codex(#[from] crate::agents::codex::oauth_usage::CodexOauthUsageErr),
}

impl OauthReportable for OpencodeOauthUsageErr {
    fn should_report(&self) -> bool {
        match self {
            Self::NoCredentials
            | Self::ApiKeyOnly
            | Self::TokenExpired
            | Self::UnsupportedProvider(_) => false,
            Self::Io(_) | Self::Parse(_) => true,
            Self::Claude(err) => err.should_report(),
            Self::Codex(err) => err.should_report(),
        }
    }
}

pub(crate) type Result<T> = std::result::Result<T, OpencodeOauthUsageErr>;

#[derive(Debug, Clone, PartialEq)]
struct SelectedCredential {
    provider: String,
    access_token: String,
    account_id: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct OpencodeCredential {
    #[serde(rename = "type")]
    kind: Option<String>,
    access: Option<String>,
    expires: Option<i64>,
    #[serde(default, rename = "accountId")]
    account_id: Option<String>,
}

pub(crate) fn fetch() -> Result<AccountUsageSnapshot> {
    let path = auth_path().ok_or(OpencodeOauthUsageErr::NoCredentials)?;
    let credential = load_credentials_from(&path, latest_message_provider())?;
    match credential.provider.as_str() {
        "anthropic" => crate::agents::claude::oauth_usage::fetch_usage_with_token(
            &credential.access_token,
            None,
        )
        .map_err(OpencodeOauthUsageErr::from),
        "openai" | "openai-codex" => crate::agents::codex::oauth_usage::fetch_usage_with_token(
            &credential.access_token,
            credential.account_id.as_deref(),
        )
        .map_err(OpencodeOauthUsageErr::from),
        provider => Err(OpencodeOauthUsageErr::UnsupportedProvider(
            provider.to_owned(),
        )),
    }
}

pub(crate) fn credentials_stamp() -> Option<u64> {
    auth_path().and_then(|path| file_mtime_ms(&path))
}

fn auth_path() -> Option<PathBuf> {
    opencode_data_dirs()
        .into_iter()
        .map(|dir| dir.join("auth.json"))
        .find(|path| path.exists())
}

fn load_credentials_from(path: &Path, used_provider: Option<String>) -> Result<SelectedCredential> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(OpencodeOauthUsageErr::NoCredentials);
        }
        Err(err) => return Err(OpencodeOauthUsageErr::Io(err)),
    };
    select_credential(&bytes, used_provider.as_deref())
}

fn select_credential(bytes: &[u8], used_provider: Option<&str>) -> Result<SelectedCredential> {
    let credentials: BTreeMap<String, OpencodeCredential> = serde_json::from_slice(bytes)?;
    let Some((provider, credential)) = used_provider
        .and_then(|provider| credentials.get_key_value(provider))
        .or_else(|| {
            credentials
                .iter()
                .find(|(_, credential)| credential.kind.as_deref() == Some("oauth"))
        })
    else {
        return Err(OpencodeOauthUsageErr::NoCredentials);
    };
    if credential.kind.as_deref() != Some("oauth") {
        return Err(OpencodeOauthUsageErr::ApiKeyOnly);
    }
    let Some(access_token) = credential
        .access
        .as_deref()
        .filter(|token| !token.is_empty())
    else {
        return Err(OpencodeOauthUsageErr::NoCredentials);
    };
    let Some(expires) = credential.expires else {
        return Err(OpencodeOauthUsageErr::TokenExpired);
    };
    if expires <= unix_now_ms() as i64 {
        return Err(OpencodeOauthUsageErr::TokenExpired);
    }
    Ok(SelectedCredential {
        provider: provider.clone(),
        access_token: access_token.to_owned(),
        account_id: credential.account_id.clone().filter(|id| !id.is_empty()),
    })
}

fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn future_ms() -> i64 {
        unix_now_ms() as i64 + 60_000
    }

    #[test]
    fn selection_prefers_used_provider_then_first_oauth() {
        let json = format!(
            r#"{{
                "anthropic": {{ "type": "oauth", "access": "anthropic-token", "expires": {} }},
                "openai": {{ "type": "oauth", "access": "openai-token", "expires": {}, "accountId": "acct_1" }}
            }}"#,
            future_ms(),
            future_ms()
        );
        let selected = select_credential(json.as_bytes(), Some("openai")).unwrap();
        assert_eq!(selected.provider, "openai");
        assert_eq!(selected.access_token, "openai-token");
        assert_eq!(selected.account_id.as_deref(), Some("acct_1"));

        // With no used-provider hint, the first OAuth credential wins.
        let selected = select_credential(json.as_bytes(), None).unwrap();
        assert_eq!(selected.provider, "anthropic");
    }

    #[test]
    fn api_key_wellknown_missing_and_expired_credentials_skip() {
        // An explicitly-selected API-key / wellknown credential is unmetered —
        // surfaced as ApiKeyOnly, never queried.
        assert!(matches!(
            select_credential(
                br#"{ "deepseek": { "type": "api", "key": "sk" } }"#,
                Some("deepseek"),
            ),
            Err(OpencodeOauthUsageErr::ApiKeyOnly)
        ));
        // With no used-provider hint and no OAuth credential, there is nothing to
        // select.
        assert!(matches!(
            select_credential(
                br#"{ "opencode": { "type": "wellknown", "key": "z" } }"#,
                None,
            ),
            Err(OpencodeOauthUsageErr::NoCredentials)
        ));
        assert!(matches!(
            select_credential(
                br#"{ "anthropic": { "type": "oauth", "access": "token", "expires": 1 } }"#,
                None,
            ),
            Err(OpencodeOauthUsageErr::TokenExpired)
        ));
        assert!(matches!(
            select_credential(br#"{}"#, None),
            Err(OpencodeOauthUsageErr::NoCredentials)
        ));
    }
}
