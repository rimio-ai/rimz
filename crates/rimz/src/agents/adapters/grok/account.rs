//! Secret-free projection of Grok Build's local authentication store.

use std::collections::BTreeMap;

use jiff::{SignedDuration, Timestamp};
use serde::Deserialize;

use crate::agents::account::{AccountProbe, file_mtime_ms};
use crate::agents::{AgentAccount, ProviderAccountScope};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AuthMode {
    #[serde(alias = "grok")]
    WebLogin,
    Oidc,
    External,
    ApiKey,
    #[default]
    #[serde(other)]
    Unknown,
}

impl AuthMode {
    const fn is_session(self) -> bool {
        matches!(self, Self::WebLogin | Self::Oidc | Self::External)
    }

    const fn label(self) -> &'static str {
        match self {
            Self::WebLogin | Self::Oidc => "Session",
            Self::External => "External",
            Self::ApiKey => "API key",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AuthRecord {
    auth_mode: AuthMode,
    create_time: Option<String>,
    user_id: String,
    email: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    principal_type: Option<String>,
    principal_id: Option<String>,
    team_id: Option<String>,
    team_name: Option<String>,
    organization_id: Option<String>,
    organization_name: Option<String>,
    expires_at: Option<String>,
    #[serde(rename = "key", deserialize_with = "deserialize_secret_presence")]
    has_key: bool,
    #[serde(
        rename = "refresh_token",
        deserialize_with = "deserialize_secret_presence"
    )]
    has_refresh_token: bool,
}

fn deserialize_secret_presence<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = Option::<String>::deserialize(deserializer)?;
    Ok(value.is_some_and(|value| !value.trim().is_empty()))
}

impl AuthRecord {
    fn created_at(&self) -> Option<Timestamp> {
        self.create_time.as_deref()?.parse().ok()
    }

    fn usable(&self, now: Timestamp) -> bool {
        if !self.has_key || self.auth_mode == AuthMode::Unknown {
            return false;
        }
        let expired = self
            .expires_at
            .as_deref()
            .and_then(|value| value.parse::<Timestamp>().ok())
            .or_else(|| {
                self.created_at()?
                    .checked_add(SignedDuration::from_hours(30 * 24))
                    .ok()
            })
            .is_some_and(|expires| expires <= now);
        !expired || self.has_refresh_token
    }

    fn account(&self, scope: &str, mtime_ms: Option<u64>) -> AgentAccount {
        let display_name = [self.first_name.as_deref(), self.last_name.as_deref()]
            .into_iter()
            .flatten()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let provider_label = self
            .team_name
            .as_deref()
            .or(self.organization_name.as_deref())
            .or(self.email.as_deref())
            .or((!display_name.is_empty()).then_some(display_name.as_str()))
            .or(self.principal_type.as_deref())
            .unwrap_or(scope)
            .trim();
        let account_id = non_empty(&self.user_id)
            .or_else(|| self.principal_id.as_deref().and_then(non_empty))
            .or_else(|| self.team_id.as_deref().and_then(non_empty))
            .or_else(|| self.organization_id.as_deref().and_then(non_empty))
            .map(ToOwned::to_owned);
        AgentAccount {
            scope: ProviderAccountScope::KindWide,
            plan: Some(self.auth_mode.label().to_owned()),
            account_id,
            metered: Some(self.auth_mode.is_session()),
            version: None,
            sub_provider: (!provider_label.is_empty()).then(|| provider_label.to_owned()),
            credentials_updated_at_ms: mtime_ms,
        }
    }
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

pub(super) fn probe() -> AccountProbe {
    let path = super::paths::auth_path();
    probe_at(
        &path,
        std::env::var_os("XAI_API_KEY").is_some_and(|value| !value.is_empty()),
        Timestamp::now(),
    )
}

fn probe_at(path: &std::path::Path, api_key_present: bool, now: Timestamp) -> AccountProbe {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => return AccountProbe::Unavailable,
    };
    if let Some(bytes) = bytes {
        let records: BTreeMap<String, AuthRecord> = match serde_json::from_slice(&bytes) {
            Ok(records) => records,
            Err(_) => return AccountProbe::Unavailable,
        };
        let mut usable = records
            .iter()
            .filter(|(_, record)| record.usable(now))
            .collect::<Vec<_>>();
        usable.sort_by(|(scope_a, a), (scope_b, b)| {
            b.auth_mode
                .is_session()
                .cmp(&a.auth_mode.is_session())
                .then_with(|| b.created_at().cmp(&a.created_at()))
                .then_with(|| scope_a.cmp(scope_b))
        });
        if let Some((scope, record)) = usable.first() {
            return AccountProbe::Found(record.account(scope, file_mtime_ms(path)));
        }
    }
    if api_key_present {
        return AccountProbe::Found(AgentAccount {
            scope: ProviderAccountScope::KindWide,
            plan: Some("API key".to_owned()),
            metered: Some(false),
            ..AgentAccount::default()
        });
    }
    AccountProbe::LoggedOut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_login_wins_and_secrets_never_enter_debug_projection() {
        let sentinel_key = "SECRET-KEY-SENTINEL";
        let sentinel_refresh = "SECRET-REFRESH-SENTINEL";
        let records: BTreeMap<String, AuthRecord> = serde_json::from_str(&format!(
            r#"{{
                "z-api": {{"key":"{sentinel_key}","auth_mode":"api_key","create_time":"2026-01-03T00:00:00Z"}},
                "a-session": {{"key":"session-token","refresh_token":"{sentinel_refresh}","auth_mode":"oidc","create_time":"2026-01-01T00:00:00Z","expires_at":"2025-01-01T00:00:00Z","user_id":"user-1","email":"dev@example.com"}}
            }}"#
        ))
        .unwrap();
        let mut usable = records.iter().collect::<Vec<_>>();
        usable.sort_by(|(scope_a, a), (scope_b, b)| {
            b.auth_mode
                .is_session()
                .cmp(&a.auth_mode.is_session())
                .then_with(|| b.created_at().cmp(&a.created_at()))
                .then_with(|| scope_a.cmp(scope_b))
        });
        let rendered = format!("{:?} {:?}", records, usable[0].1.account(usable[0].0, None));
        assert_eq!(usable[0].0.as_str(), "a-session");
        assert!(!rendered.contains(sentinel_key));
        assert!(!rendered.contains(sentinel_refresh));
    }

    #[test]
    fn file_probe_is_deterministic_and_malformed_auth_is_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        std::fs::write(
            &path,
            r#"{
                "z-api":{"key":"api-secret","auth_mode":"api_key","create_time":"2026-07-10T00:00:00Z"},
                "a-session":{"key":"session-secret","auth_mode":"oidc","create_time":"2026-07-01T00:00:00Z","refresh_token":"refresh-secret","user_id":"user-1"}
            }"#,
        )
        .unwrap();
        let now = "2026-07-20T00:00:00Z".parse().unwrap();
        let AccountProbe::Found(account) = probe_at(&path, false, now) else {
            panic!("session account expected");
        };
        assert_eq!(account.account_id.as_deref(), Some("user-1"));
        assert_eq!(account.metered, Some(true));

        std::fs::write(&path, "{").unwrap();
        assert!(matches!(
            probe_at(&path, true, now),
            AccountProbe::Unavailable
        ));
        std::fs::remove_file(&path).unwrap();
        let AccountProbe::Found(account) = probe_at(&path, true, now) else {
            panic!("environment API key fallback expected");
        };
        assert_eq!(account.metered, Some(false));
    }
}
