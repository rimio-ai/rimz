//! Pi OAuth account-usage probe.
//!
//! Pi's `auth.json` stores provider OAuth tokens for the backing subscriptions.
//! The quota surfaces are the same provider APIs Claude and Codex already parse,
//! so this module only selects Pi's active OAuth credential and delegates the
//! fetch to the sibling adapter.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::agents::account::file_mtime_ms;
use crate::agents::{AccountUsageSnapshot, ProviderAccountScope};

use super::account;
use super::spend::pi_config_dir;

const ACCOUNT_KEY_DOMAIN: &[u8] = b"rimz/pi-oauth-account-key/v1";

#[derive(Debug, thiserror::Error)]
pub(crate) enum PiOauthUsageErr {
    #[error("pi OAuth credentials not found")]
    NoCredentials,
    #[error("pi auth file selected an API-key credential")]
    ApiKeyOnly,
    #[error("pi OAuth token is expired")]
    TokenExpired,
    #[error("pi OAuth usage is unsupported for provider `{0}`")]
    UnsupportedProvider(String),
    #[error("reading pi OAuth credentials: {0}")]
    Io(#[from] std::io::Error),
    #[error("parsing pi OAuth credentials: {0}")]
    Parse(#[from] serde_json::Error),
    #[error(transparent)]
    Claude(#[from] crate::agents::claude::oauth_usage::ClaudeOauthUsageErr),
    #[error(transparent)]
    Codex(#[from] crate::agents::codex::oauth_usage::CodexOauthUsageErr),
}

impl crate::agents::credits::OauthReportable for PiOauthUsageErr {
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

pub(crate) type Result<T> = std::result::Result<T, PiOauthUsageErr>;

#[derive(Clone, PartialEq)]
struct SelectedCredential {
    provider: String,
    access_token: String,
    account_key: String,
    scope: ProviderAccountScope,
    account_id: Option<String>,
}

#[derive(Default, Deserialize)]
struct PiCredential {
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
        .map_err(PiOauthUsageErr::from),
        "openai" | "openai-codex" => crate::agents::codex::oauth_usage::fetch_usage_with_token(
            &credential.access_token,
            credential.account_id.as_deref(),
        )
        .map_err(PiOauthUsageErr::from),
        provider => Err(PiOauthUsageErr::UnsupportedProvider(provider.to_owned())),
    }?;
    Ok(with_selected_identity(snapshot, &credential))
}

pub(crate) fn account_usage_identity() -> crate::agents::AccountUsageIdentity {
    let credentials_stamp = file_mtime_ms(&pi_config_dir().join("auth.json"));
    let Ok(credential) = current_credential() else {
        return crate::agents::AccountUsageIdentity {
            credentials_stamp,
            ..Default::default()
        };
    };
    crate::agents::AccountUsageIdentity {
        scope: credential.scope,
        account_key: Some(credential.account_key),
        credentials_stamp,
    }
}

fn current_credential() -> Result<SelectedCredential> {
    load_credentials_from(&pi_config_dir().join("auth.json"), account::used_provider())
}

fn load_credentials_from(path: &Path, used_provider: Option<String>) -> Result<SelectedCredential> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Err(PiOauthUsageErr::NoCredentials);
        }
        Err(err) => return Err(PiOauthUsageErr::Io(err)),
    };
    select_credential(&bytes, used_provider.as_deref())
}

fn select_credential(bytes: &[u8], used_provider: Option<&str>) -> Result<SelectedCredential> {
    let credentials: BTreeMap<String, PiCredential> = serde_json::from_slice(bytes)?;
    let Some((provider, credential)) = used_provider
        .and_then(|provider| credentials.get_key_value(provider))
        .or_else(|| {
            credentials
                .iter()
                .find(|(_, credential)| credential.kind.as_deref() == Some("oauth"))
        })
    else {
        return Err(PiOauthUsageErr::NoCredentials);
    };
    if credential.kind.as_deref() != Some("oauth") {
        return Err(PiOauthUsageErr::ApiKeyOnly);
    }
    let Some(access_token) = credential
        .access
        .as_deref()
        .filter(|token| !token.is_empty())
    else {
        return Err(PiOauthUsageErr::NoCredentials);
    };
    let Some(expires) = credential.expires else {
        return Err(PiOauthUsageErr::TokenExpired);
    };
    if expires <= unix_now_ms() as i64 {
        return Err(PiOauthUsageErr::TokenExpired);
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
                "openai-codex": {{ "type": "oauth", "access": "openai-token", "expires": {}, "accountId": "acct_1" }}
            }}"#,
            future_ms(),
            future_ms()
        );
        let selected = select_credential(json.as_bytes(), Some("openai-codex")).unwrap();
        assert_eq!(selected.provider, "openai-codex");
        assert_eq!(selected.access_token, "openai-token");
        assert_eq!(selected.account_id.as_deref(), Some("acct_1"));
        assert_eq!(selected.account_key, "acct_1");
        assert_eq!(
            selected.scope,
            ProviderAccountScope::sub_provider("openai", "oauth")
        );

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
    fn api_key_missing_and_expired_credentials_skip() {
        assert!(matches!(
            select_credential(br#"{ "openai": { "type": "api_key", "key": "sk" } }"#, None),
            Err(PiOauthUsageErr::NoCredentials)
        ));
        assert!(matches!(
            select_credential(
                br#"{ "anthropic": { "type": "oauth", "access": "token", "expires": 1 } }"#,
                None,
            ),
            Err(PiOauthUsageErr::TokenExpired)
        ));
        assert!(matches!(
            select_credential(br#"{}"#, None),
            Err(PiOauthUsageErr::NoCredentials)
        ));
    }
}
