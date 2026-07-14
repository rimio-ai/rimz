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
use sha2::{Digest, Sha256};

use crate::agents::credits::{OauthReportable, file_mtime_ms};
use crate::agents::{AccountUsageSnapshot, ProviderAccountScope};

use super::account;
use super::spend::{latest_message_provider, opencode_data_dirs};

const ACCOUNT_KEY_DOMAIN: &[u8] = b"rimz/opencode-oauth-account-key/v1";

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

#[derive(Clone, PartialEq)]
struct SelectedCredential {
    provider: String,
    access_token: String,
    account_key: String,
    scope: ProviderAccountScope,
    account_id: Option<String>,
}

#[derive(Default, Deserialize)]
struct OpencodeCredential {
    #[serde(rename = "type")]
    kind: Option<String>,
    access: Option<String>,
    refresh: Option<String>,
    expires: Option<i64>,
    #[serde(default, rename = "accountId")]
    account_id: Option<String>,
}

pub(crate) fn fetch() -> Result<AccountUsageSnapshot> {
    let credential = current_credential()?;
    let snapshot = match credential.provider.as_str() {
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
    }?;
    Ok(with_selected_identity(snapshot, &credential))
}

pub(crate) fn credentials_stamp() -> Option<u64> {
    auth_path().and_then(|path| file_mtime_ms(&path))
}

pub(crate) fn current_account_key() -> Option<String> {
    current_credential()
        .ok()
        .map(|credential| credential.account_key)
}

pub(crate) fn current_account_scope() -> ProviderAccountScope {
    current_credential()
        .map(|credential| credential.scope)
        .unwrap_or_default()
}

fn current_credential() -> Result<SelectedCredential> {
    let path = auth_path().ok_or(OpencodeOauthUsageErr::NoCredentials)?;
    load_credentials_from(&path, latest_message_provider())
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
    let account_id = credential.account_id.clone().filter(|id| !id.is_empty());
    let refresh_token = credential
        .refresh
        .as_deref()
        .filter(|token| !token.is_empty());
    let scope = account::oauth_scope(provider).unwrap_or_default();
    let fallback_account_key = hashed_account_key(
        refresh_token.map_or("access-token", |_| "refresh-token"),
        refresh_token.unwrap_or(access_token),
    );
    let account_key = if scope == ProviderAccountScope::sub_provider("openai", "oauth") {
        account_id.clone().unwrap_or(fallback_account_key)
    } else {
        fallback_account_key
    };
    Ok(SelectedCredential {
        provider: provider.clone(),
        access_token: access_token.to_owned(),
        account_key,
        scope,
        account_id,
    })
}

fn with_selected_identity(
    mut snapshot: AccountUsageSnapshot,
    credential: &SelectedCredential,
) -> AccountUsageSnapshot {
    snapshot.account_key = Some(credential.account_key.clone());
    snapshot.scope = credential.scope.clone();
    snapshot
}

fn hashed_account_key(secret_kind: &str, secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ACCOUNT_KEY_DOMAIN);
    hasher.update([0]);
    hasher.update(secret_kind.as_bytes());
    hasher.update([0]);
    hasher.update(secret.as_bytes());
    hex::encode(hasher.finalize())
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
        assert_eq!(selected.account_key, "acct_1");
        assert_eq!(
            selected.scope,
            ProviderAccountScope::sub_provider("openai", "oauth")
        );

        // With no used-provider hint, the first OAuth credential wins.
        let selected = select_credential(json.as_bytes(), None).unwrap();
        assert_eq!(selected.provider, "anthropic");
        assert_eq!(
            selected.scope,
            ProviderAccountScope::sub_provider("anthropic", "oauth")
        );
    }

    #[test]
    fn token_identity_prefers_refresh_and_is_applied_to_the_snapshot() {
        let first = format!(
            r#"{{ "anthropic": {{ "type": "oauth", "access": "access-1", "refresh": "stable-refresh", "expires": {} }} }}"#,
            future_ms()
        );
        let rotated = format!(
            r#"{{ "anthropic": {{ "type": "oauth", "access": "access-2", "refresh": "stable-refresh", "expires": {} }} }}"#,
            future_ms()
        );
        let first = select_credential(first.as_bytes(), None).unwrap();
        let rotated = select_credential(rotated.as_bytes(), None).unwrap();
        let access_only = format!(
            r#"{{ "anthropic": {{ "type": "oauth", "access": "access-only", "expires": {} }} }}"#,
            future_ms()
        );
        let access_only = select_credential(access_only.as_bytes(), None).unwrap();
        assert_eq!(first.account_key, rotated.account_key);
        assert_ne!(first.account_key, access_only.account_key);
        assert_eq!(first.account_key.len(), 64);
        assert_eq!(access_only.account_key.len(), 64);
        for secret in ["access-1", "access-2", "access-only", "stable-refresh"] {
            assert!(!first.account_key.contains(secret));
            assert!(!access_only.account_key.contains(secret));
        }

        let snapshot = with_selected_identity(AccountUsageSnapshot::default(), &first);
        assert_eq!(
            snapshot.account_key.as_deref(),
            Some(first.account_key.as_str())
        );
        assert_eq!(snapshot.scope, first.scope);
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
