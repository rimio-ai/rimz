//! Shared structured `auth.json` account seam for Pi and OpenCode.
//!
//! Both adapters store provider credentials in the same map shape and delegate
//! Anthropic/OpenAI quota reads to RimZ's provider-owned normalizers.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::account::{AccountProbe, file_mtime_ms};
use super::credits::AccountUsageReportable;
use super::{AccountUsageIdentity, AccountUsageProbe, AgentAccount, ProviderAccountScope};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Adapter {
    Pi,
    OpenCode,
}

impl Adapter {
    fn name(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::OpenCode => "opencode",
        }
    }
}

pub(crate) struct Config {
    pub(crate) adapter: Adapter,
    pub(crate) auth_path: Option<PathBuf>,
    pub(crate) used_provider: fn() -> Option<String>,
    pub(crate) api_key_types: &'static [&'static str],
    pub(crate) account_key_domain: &'static [u8],
}

#[derive(Debug, Default, Deserialize)]
struct Credential {
    #[serde(rename = "type")]
    kind: Option<String>,
    access: Option<String>,
    refresh: Option<String>,
    expires: Option<i64>,
    #[serde(default, rename = "accountId")]
    account_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct SelectedCredential {
    provider: String,
    access_token: String,
    account_key: String,
    scope: ProviderAccountScope,
    account_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("{adapter} OAuth credentials not found")]
    NoCredentials { adapter: &'static str },
    #[error("{adapter} auth file selected an API-key credential")]
    ApiKeyOnly { adapter: &'static str },
    #[error("{adapter} OAuth token is expired")]
    TokenExpired { adapter: &'static str },
    #[error("{adapter} OAuth usage is unsupported for provider `{provider}`")]
    UnsupportedProvider {
        adapter: &'static str,
        provider: String,
    },
    #[error("reading {adapter} OAuth credentials: {source}")]
    Io {
        adapter: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {adapter} OAuth credentials: {source}")]
    Parse {
        adapter: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("{adapter}: {source}")]
    Claude {
        adapter: &'static str,
        #[source]
        source: super::claude::oauth_usage::ClaudeOauthUsageErr,
    },
    #[error("{adapter}: {source}")]
    Codex {
        adapter: &'static str,
        #[source]
        source: super::codex::oauth_usage::CodexOauthUsageErr,
    },
}

impl AccountUsageReportable for Error {
    fn should_report(&self) -> bool {
        match self {
            Self::NoCredentials { .. }
            | Self::ApiKeyOnly { .. }
            | Self::TokenExpired { .. }
            | Self::UnsupportedProvider { .. } => false,
            Self::Io { .. } | Self::Parse { .. } => true,
            Self::Claude { source, .. } => source.should_report(),
            Self::Codex { source, .. } => source.should_report(),
        }
    }
}

pub(crate) fn probe_account(config: &Config) -> AccountProbe {
    let Some(path) = config.auth_path.as_deref() else {
        return AccountProbe::LoggedOut;
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return AccountProbe::LoggedOut,
        Err(_) => return AccountProbe::Unavailable,
    };
    let Ok(credentials) = serde_json::from_slice::<BTreeMap<String, Credential>>(&bytes) else {
        return AccountProbe::Unavailable;
    };
    if credentials.is_empty() {
        return AccountProbe::LoggedOut;
    }
    let used_provider = (config.used_provider)();
    let Some((provider, credential)) = select_display(&credentials, used_provider.as_deref())
    else {
        return AccountProbe::LoggedOut;
    };
    let kind = credential.kind.as_deref();
    let scope = (kind == Some("oauth"))
        .then(|| oauth_scope(provider))
        .flatten()
        .unwrap_or_default();
    AccountProbe::Found(AgentAccount {
        scope,
        plan: Some(provider_label(config, provider, kind)),
        metered: if kind == Some("oauth") {
            Some(true)
        } else if kind.is_some_and(|kind| config.api_key_types.contains(&kind)) {
            Some(false)
        } else {
            None
        },
        sub_provider: Some(provider.clone()),
        credentials_updated_at_ms: file_mtime_ms(path),
        ..Default::default()
    })
}

pub(crate) fn probe_account_usage(config: &Config) -> AccountUsageProbe {
    let stamp = config.auth_path.as_deref().and_then(file_mtime_ms);
    let selected = select_usage_from_config(config);
    let selected = match selected {
        Ok(selected) => selected,
        Err(error) => {
            return super::credits::map_account_usage_probe(
                Err(error),
                AccountUsageIdentity {
                    credentials_stamp: stamp,
                    ..Default::default()
                },
                config.adapter.name(),
            );
        }
    };
    let identity = AccountUsageIdentity {
        scope: selected.scope.clone(),
        account_key: Some(selected.account_key.clone()),
        credentials_stamp: stamp,
    };
    let result = match selected.provider.as_str() {
        "anthropic" => {
            super::claude::oauth_usage::fetch_usage_with_token(&selected.access_token, None)
                .map_err(|source| Error::Claude {
                    adapter: config.adapter.name(),
                    source,
                })
        }
        "openai" | "openai-codex" => super::codex::oauth_usage::fetch_usage_with_token(
            &selected.access_token,
            selected.account_id.as_deref(),
        )
        .map_err(|source| Error::Codex {
            adapter: config.adapter.name(),
            source,
        }),
        provider => Err(Error::UnsupportedProvider {
            adapter: config.adapter.name(),
            provider: provider.to_owned(),
        }),
    };
    super::credits::map_account_usage_probe(result, identity, config.adapter.name())
}

fn select_display<'a>(
    credentials: &'a BTreeMap<String, Credential>,
    used_provider: Option<&str>,
) -> Option<(&'a String, &'a Credential)> {
    used_provider
        .and_then(|provider| credentials.get_key_value(provider))
        .or_else(|| {
            credentials
                .iter()
                .find(|(_, credential)| credential.kind.as_deref() == Some("oauth"))
        })
        .or_else(|| credentials.iter().next())
}

fn select_usage_from_config(config: &Config) -> Result<SelectedCredential, Error> {
    let path = config.auth_path.as_deref().ok_or(Error::NoCredentials {
        adapter: config.adapter.name(),
    })?;
    let bytes = std::fs::read(path).map_err(|source| {
        if source.kind() == std::io::ErrorKind::NotFound {
            Error::NoCredentials {
                adapter: config.adapter.name(),
            }
        } else {
            Error::Io {
                adapter: config.adapter.name(),
                source,
            }
        }
    })?;
    select_usage(&bytes, config)
}

fn select_usage(bytes: &[u8], config: &Config) -> Result<SelectedCredential, Error> {
    let credentials: BTreeMap<String, Credential> =
        serde_json::from_slice(bytes).map_err(|source| Error::Parse {
            adapter: config.adapter.name(),
            source,
        })?;
    if credentials.is_empty() {
        return Err(Error::NoCredentials {
            adapter: config.adapter.name(),
        });
    }
    let used_provider = (config.used_provider)();
    let Some((provider, credential)) = used_provider
        .as_deref()
        .and_then(|provider| credentials.get_key_value(provider))
        .or_else(|| {
            credentials
                .iter()
                .find(|(_, credential)| credential.kind.as_deref() == Some("oauth"))
        })
    else {
        return Err(Error::NoCredentials {
            adapter: config.adapter.name(),
        });
    };
    if credential.kind.as_deref() != Some("oauth") {
        return Err(Error::ApiKeyOnly {
            adapter: config.adapter.name(),
        });
    }
    let access_token = credential
        .access
        .clone()
        .filter(|token| !token.is_empty())
        .ok_or(Error::NoCredentials {
            adapter: config.adapter.name(),
        })?;
    if credential
        .expires
        .is_none_or(|expires| expires <= unix_now_ms() as i64)
    {
        return Err(Error::TokenExpired {
            adapter: config.adapter.name(),
        });
    }
    let account_id = credential.account_id.clone().filter(|id| !id.is_empty());
    let refresh_token = credential
        .refresh
        .as_deref()
        .filter(|token| !token.is_empty());
    let scope = oauth_scope(provider).unwrap_or_default();
    let fallback = hashed_account_key(
        config.account_key_domain,
        refresh_token
            .as_ref()
            .map_or("access-token", |_| "refresh-token"),
        refresh_token.unwrap_or(&access_token),
    );
    let account_key = if scope == ProviderAccountScope::sub_provider("openai", "oauth") {
        account_id.clone().unwrap_or(fallback)
    } else {
        fallback
    };
    Ok(SelectedCredential {
        provider: provider.clone(),
        access_token,
        account_key,
        scope,
        account_id,
    })
}

fn oauth_scope(provider: &str) -> Option<ProviderAccountScope> {
    match provider {
        "openai" | "openai-codex" => Some(ProviderAccountScope::sub_provider("openai", "oauth")),
        "anthropic" => Some(ProviderAccountScope::sub_provider("anthropic", "oauth")),
        _ => None,
    }
}

fn provider_label(config: &Config, provider: &str, kind: Option<&str>) -> String {
    let name = match (config.adapter, provider) {
        (Adapter::OpenCode, "opencode") => "OpenCode",
        (Adapter::OpenCode, "deepseek") => "DeepSeek",
        (_, provider) => match provider {
            "anthropic" => "Anthropic",
            "openai" | "openai-codex" => "OpenAI",
            "github-copilot" => "GitHub Copilot",
            "google" | "gemini" => "Google",
            other => other,
        },
    };
    match kind {
        Some("oauth") => format!("{name} OAuth"),
        Some(kind) if config.api_key_types.contains(&kind) => format!("{name} API Key"),
        Some("wellknown") if config.adapter == Adapter::OpenCode => format!("{name} Wellknown"),
        _ => name.to_owned(),
    }
}

fn hashed_account_key(domain: &[u8], secret_kind: &str, secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain);
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn no_used_provider() -> Option<String> {
        None
    }

    fn openai_used_provider() -> Option<String> {
        Some("openai".to_owned())
    }

    fn opencode_used_provider() -> Option<String> {
        Some("opencode".to_owned())
    }

    fn config(adapter: Adapter, used: Option<&str>, domain: &'static [u8]) -> Config {
        Config {
            adapter,
            auth_path: None,
            used_provider: match used {
                Some("openai") => openai_used_provider,
                Some("opencode") => opencode_used_provider,
                Some(other) => panic!("missing test resolver for {other}"),
                None => no_used_provider,
            },
            api_key_types: match adapter {
                Adapter::Pi => &["api_key"],
                Adapter::OpenCode => &["api", "api_key"],
            },
            account_key_domain: domain,
        }
    }

    fn future_ms() -> u64 {
        unix_now_ms() + 60_000
    }

    #[test]
    fn pi_opencode_display_selection_preserves_priority_labels_and_metering() {
        let credentials: BTreeMap<String, Credential> = serde_json::from_str(
            r#"{
                "anthropic":{"type":"oauth"},
                "openai":{"type":"api"},
                "opencode":{"type":"wellknown"}
            }"#,
        )
        .unwrap();
        let selected = select_display(&credentials, Some("openai")).unwrap();
        assert_eq!(selected.0, "openai");
        assert_eq!(
            provider_label(
                &config(Adapter::OpenCode, None, b"open"),
                selected.0,
                selected.1.kind.as_deref()
            ),
            "OpenAI API Key"
        );
        assert_eq!(
            provider_label(
                &config(Adapter::OpenCode, None, b"open"),
                "opencode",
                Some("wellknown")
            ),
            "OpenCode Wellknown"
        );
        assert_eq!(
            provider_label(&config(Adapter::Pi, None, b"pi"), "custom", None),
            "custom"
        );
        assert_eq!(
            provider_label(&config(Adapter::Pi, None, b"pi"), "opencode", None),
            "opencode"
        );
        assert_eq!(select_display(&credentials, None).unwrap().0, "anthropic");
    }

    #[test]
    fn pi_opencode_usage_selection_keeps_scope_expiry_identity_and_adapter_domains() {
        let json = format!(
            r#"{{
                "anthropic":{{"type":"oauth","access":"a","refresh":"stable","expires":{}}},
                "openai":{{"type":"oauth","access":"o","expires":{},"accountId":"acct"}}
            }}"#,
            future_ms(),
            future_ms()
        );
        let pi =
            select_usage(json.as_bytes(), &config(Adapter::Pi, Some("openai"), b"pi")).unwrap();
        assert_eq!(pi.account_key, "acct");
        assert_eq!(
            pi.scope,
            ProviderAccountScope::sub_provider("openai", "oauth")
        );

        let first = select_usage(json.as_bytes(), &config(Adapter::Pi, None, b"pi")).unwrap();
        let rotated = format!(
            r#"{{"anthropic":{{"type":"oauth","access":"rotated","refresh":"stable","expires":{}}}}}"#,
            future_ms()
        );
        let rotated = select_usage(rotated.as_bytes(), &config(Adapter::Pi, None, b"pi")).unwrap();
        let access_only = format!(
            r#"{{"anthropic":{{"type":"oauth","access":"access-only","expires":{}}}}}"#,
            future_ms()
        );
        let access_only =
            select_usage(access_only.as_bytes(), &config(Adapter::Pi, None, b"pi")).unwrap();
        let other = select_usage(
            json.as_bytes(),
            &config(Adapter::OpenCode, None, b"opencode"),
        )
        .unwrap();
        assert_eq!(first.provider, "anthropic");
        assert_eq!(first.account_key, rotated.account_key);
        assert_ne!(first.account_key, access_only.account_key);
        assert_ne!(first.account_key, other.account_key);
        assert_eq!(first.account_key.len(), 64);
        assert!(!first.account_key.contains("stable"));

        let expired = br#"{"anthropic":{"type":"oauth","access":"a","expires":1}}"#;
        assert!(matches!(
            select_usage(expired, &config(Adapter::Pi, None, b"pi")),
            Err(Error::TokenExpired { .. })
        ));
    }

    #[test]
    fn pi_opencode_usage_selection_handles_api_key_unsupported_empty_and_malformed_maps() {
        assert!(matches!(
            select_usage(
                br#"{"openai":{"type":"api_key","key":"k"}}"#,
                &config(Adapter::Pi, Some("openai"), b"pi")
            ),
            Err(Error::ApiKeyOnly { .. })
        ));
        assert!(matches!(
            select_usage(br#"{}"#, &config(Adapter::OpenCode, None, b"open")),
            Err(Error::NoCredentials { .. })
        ));
        assert!(matches!(
            select_usage(b"not json", &config(Adapter::Pi, None, b"pi")),
            Err(Error::Parse { .. })
        ));
    }

    #[test]
    fn pi_opencode_account_probe_handles_files_and_adapter_specific_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut pi = config(Adapter::Pi, Some("openai"), b"pi");
        pi.auth_path = Some(path.clone());
        assert!(matches!(probe_account(&pi), AccountProbe::LoggedOut));

        std::fs::write(&path, b"not json").unwrap();
        assert!(matches!(probe_account(&pi), AccountProbe::Unavailable));

        std::fs::write(
            &path,
            br#"{
                "anthropic":{"type":"oauth"},
                "openai":{"type":"api_key"},
                "opencode":{"type":"wellknown"}
            }"#,
        )
        .unwrap();
        let AccountProbe::Found(account) = probe_account(&pi) else {
            panic!("pi account")
        };
        assert_eq!(account.plan.as_deref(), Some("OpenAI API Key"));
        assert_eq!(account.metered, Some(false));
        assert!(account.credentials_updated_at_ms.is_some());

        let mut opencode = config(Adapter::OpenCode, Some("opencode"), b"open");
        opencode.auth_path = Some(path);
        let AccountProbe::Found(account) = probe_account(&opencode) else {
            panic!("opencode account")
        };
        assert_eq!(account.plan.as_deref(), Some("OpenCode Wellknown"));
        assert_eq!(account.metered, None);
    }

    static RESOLVER_CALLS: AtomicUsize = AtomicUsize::new(0);

    fn counted_openai_provider() -> Option<String> {
        RESOLVER_CALLS.fetch_add(1, Ordering::Relaxed);
        Some("openai".to_owned())
    }

    #[test]
    fn active_provider_resolves_only_after_valid_nonempty_auth() {
        RESOLVER_CALLS.store(0, Ordering::Relaxed);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        let mut config = config(Adapter::OpenCode, None, b"open");
        config.auth_path = Some(path.clone());
        config.used_provider = counted_openai_provider;

        assert!(matches!(probe_account(&config), AccountProbe::LoggedOut));
        std::fs::write(&path, b"not json").unwrap();
        assert!(matches!(probe_account(&config), AccountProbe::Unavailable));
        std::fs::write(&path, b"{}").unwrap();
        assert!(matches!(probe_account(&config), AccountProbe::LoggedOut));
        assert_eq!(RESOLVER_CALLS.load(Ordering::Relaxed), 0);

        std::fs::write(
            &path,
            br#"{"anthropic":{"type":"oauth"},"openai":{"type":"api_key"}}"#,
        )
        .unwrap();
        let AccountProbe::Found(account) = probe_account(&config) else {
            panic!("valid credentials should select an account")
        };
        assert_eq!(account.sub_provider.as_deref(), Some("openai"));
        assert_eq!(account.plan.as_deref(), Some("OpenAI API Key"));
        assert_eq!(RESOLVER_CALLS.load(Ordering::Relaxed), 1);
    }
}
