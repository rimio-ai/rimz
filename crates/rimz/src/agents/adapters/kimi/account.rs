//! Best-effort Kimi Code OAuth account probe.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agents::account::AccountProbe;
use crate::agents::context::AgentAccount;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CredentialShape {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_at: Option<f64>,
}

/// The `[providers."managed:kimi-code"]` entry `kimi login` writes to `config.toml`.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub(super) struct ManagedProvider {
    pub(super) base_url: Option<String>,
    oauth: Option<OAuthRef>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct OAuthRef {
    key: Option<String>,
}

impl ManagedProvider {
    /// The token file name for this provider's OAuth key: `oauth/kimi-code` is the
    /// default slot, and any other host and base pair (such as a kimi.ai login)
    /// stores its token under `oauth/kimi-code-env-<hex>`.
    fn credential_name(&self) -> &str {
        self.oauth
            .as_ref()
            .and_then(|oauth| oauth.key.as_deref())
            .map(|key| key.strip_prefix("oauth/").unwrap_or(key))
            .filter(|name| {
                !name.is_empty()
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
            .unwrap_or(DEFAULT_CREDENTIAL_NAME)
    }
}

const DEFAULT_CREDENTIAL_NAME: &str = "kimi-code";

pub(super) fn managed_provider() -> Option<ManagedProvider> {
    managed_provider_at(&super::install::config_path().ok()?)
}

fn managed_provider_at(path: &Path) -> Option<ManagedProvider> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut root: toml::Table = toml::from_str(&text).ok()?;
    root.remove("providers")?
        .as_table_mut()?
        .remove("managed:kimi-code")?
        .try_into()
        .ok()
}

pub(super) fn credentials_path() -> PathBuf {
    credentials_path_under(&super::wire::kimi_home(), managed_provider().as_ref())
}

fn credentials_path_under(home: &Path, provider: Option<&ManagedProvider>) -> PathBuf {
    let name = provider.map_or(DEFAULT_CREDENTIAL_NAME, ManagedProvider::credential_name);
    home.join("credentials").join(format!("{name}.json"))
}

pub(super) fn probe() -> AccountProbe {
    probe_at(&credentials_path())
}

fn probe_at(path: &Path) -> AccountProbe {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return AccountProbe::LoggedOut;
        }
        Err(_) => return AccountProbe::Unavailable,
    };
    let Ok(shape) = serde_json::from_slice::<CredentialShape>(&bytes) else {
        return AccountProbe::Unavailable;
    };
    let has_access = shape
        .access_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty());
    let refreshable = shape
        .refresh_token
        .as_deref()
        .is_some_and(|token| !token.trim().is_empty());
    if !has_access && !refreshable {
        return AccountProbe::LoggedOut;
    }
    let access_fresh = has_access
        && shape.expires_at.is_some_and(|seconds| {
            seconds.is_finite() && seconds > jiff::Timestamp::now().as_second() as f64
        });
    if !refreshable && !access_fresh {
        return AccountProbe::LoggedOut;
    }
    AccountProbe::Found(AgentAccount {
        scope: Default::default(),
        plan: Some("Code".to_owned()),
        account_id: None,
        metered: Some(true),
        version: None,
        sub_provider: None,
        credentials_updated_at_ms: crate::agents::account::file_mtime_ms(path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_slot_follows_the_managed_provider_oauth_key() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        std::fs::write(
            &config,
            r#"
[providers."managed:kimi-code"]
type = "kimi"
base_url = "https://api.kimi.ai/coding/v1"
api_key = ""
oauth = { storage = "file", key = "oauth/kimi-code-env-0123456789abcdef", oauth_host = "https://auth.kimi.ai" }
"#,
        )
        .unwrap();
        let provider = managed_provider_at(&config).unwrap();
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.kimi.ai/coding/v1")
        );
        assert_eq!(
            credentials_path_under(dir.path(), Some(&provider)),
            dir.path()
                .join("credentials/kimi-code-env-0123456789abcdef.json")
        );

        let default_slot = dir.path().join("credentials/kimi-code.json");
        assert_eq!(credentials_path_under(dir.path(), None), default_slot);
        std::fs::write(
            &config,
            "[providers.\"managed:kimi-code\"]\noauth = { key = \"oauth/../../escape\" }\n",
        )
        .unwrap();
        let escaping = managed_provider_at(&config).unwrap();
        assert_eq!(
            credentials_path_under(dir.path(), Some(&escaping)),
            default_slot
        );
    }

    #[test]
    fn file_login_carries_credential_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kimi-code.json");
        std::fs::write(&path, r#"{"refresh_token":"refresh"}"#).unwrap();
        let AccountProbe::Found(account) = probe_at(&path) else {
            panic!("refreshable credential must report an account");
        };
        assert!(account.credentials_updated_at_ms.is_some());
    }

    #[test]
    fn refresh_token_preserves_login_after_access_expiry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("kimi-code.json");
        let now = jiff::Timestamp::now().as_second();
        std::fs::write(
            &path,
            format!(
                r#"{{"access_token":"expired","refresh_token":"refresh","expires_at":{}}}"#,
                now - 1
            ),
        )
        .unwrap();
        assert!(matches!(probe_at(&path), AccountProbe::Found(_)));

        std::fs::write(
            &path,
            format!(r#"{{"access_token":"expired","expires_at":{}}}"#, now - 1),
        )
        .unwrap();
        assert!(matches!(probe_at(&path), AccountProbe::LoggedOut));
    }
}
