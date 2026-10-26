//! Shared credential-loader account seam for Pi and OpenCode.
//!
//! Both adapters load provider credentials into the same map shape and delegate
//! Anthropic/OpenAI quota reads to RimZ's provider-owned normalizers. OpenCode
//! also delegates Console Go status reads.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::utils::time::unix_now_ms;

use super::account::{AccountProbe, AccountProbeError, file_mtime_ms};
use super::credits::{AccountUsageFault, AccountUsageReportable};
use super::{AccountUsageIdentity, AccountUsageProbe, AgentAccount, ProviderAccountScope};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Adapter {
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

    pub(super) fn has_quota_surface(self, provider: &str) -> bool {
        quota_surface(self, provider).is_some()
    }
}

pub(super) struct Config {
    pub(crate) adapter: Adapter,
    pub(crate) load_credentials: Box<dyn Fn() -> Credentials>,
    pub(crate) used_provider: Box<dyn Fn() -> Option<String>>,
    pub(crate) api_key_types: &'static [&'static str],
    pub(crate) account_key_domain: &'static [u8],
}

pub(super) struct Credentials {
    pub(super) stamp: Option<u64>,
    pub(super) entries: Result<BTreeMap<String, Credential>, Error>,
}

#[derive(Debug, Default, Deserialize)]
pub(super) struct Credential {
    #[serde(rename = "type")]
    kind: Option<String>,
    access: Option<String>,
    refresh: Option<String>,
    expires: Option<i64>,
    #[serde(default, rename = "accountId")]
    account_id: Option<String>,
    #[serde(default)]
    metadata: CredentialMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct CredentialMetadata {
    #[serde(rename = "accountID")]
    account_id: Option<String>,
    #[serde(rename = "orgID")]
    org_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
struct SelectedCredential {
    provider: String,
    access_token: String,
    account_key: String,
    scope: ProviderAccountScope,
    account_id: Option<String>,
    org_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
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
        source: super::adapters::claude::oauth_usage::ClaudeOauthUsageErr,
    },
    #[error("{adapter}: {source}")]
    Codex {
        adapter: &'static str,
        #[source]
        source: super::adapters::codex::oauth_usage::CodexOauthUsageErr,
    },
    #[error("{adapter}: {source}")]
    Go {
        adapter: &'static str,
        #[source]
        source: super::adapters::opencode::go_usage::GoUsageErr,
    },
}

impl AccountUsageReportable for Error {
    fn fault(&self) -> AccountUsageFault {
        match self {
            Self::NoCredentials { .. }
            | Self::ApiKeyOnly { .. }
            | Self::TokenExpired { .. }
            | Self::UnsupportedProvider { .. } => AccountUsageFault::NoCredentials,
            Self::Io { .. } | Self::Parse { .. } => AccountUsageFault::Transient,
            Self::Claude { source, .. } => source.fault(),
            Self::Codex { source, .. } => source.fault(),
            Self::Go { source, .. } => source.fault(),
        }
    }
}

pub(super) fn probe_account(config: &Config) -> AccountProbe {
    let loaded = (config.load_credentials)();
    let credentials = match loaded.entries {
        Ok(credentials) => credentials,
        Err(Error::NoCredentials { .. }) => return AccountProbe::LoggedOut,
        Err(Error::Io { .. }) => return AccountProbe::Unavailable(AccountProbeError::ReadFailed),
        Err(_) => return AccountProbe::Unavailable(AccountProbeError::UnparseableOutput),
    };
    if credentials.is_empty() {
        return AccountProbe::LoggedOut;
    }
    let used_provider = (config.used_provider)();
    let Some((provider, credential)) =
        select_display(&credentials, used_provider.as_deref(), config.adapter)
    else {
        return AccountProbe::LoggedOut;
    };
    let kind = credential.kind.as_deref();
    let scope = (kind == Some("oauth"))
        .then(|| oauth_scope(config.adapter, provider))
        .flatten()
        .unwrap_or_default();
    AccountProbe::Found(AgentAccount {
        scope,
        plan: match (config.adapter, provider.as_str(), kind) {
            (Adapter::OpenCode, "opencode", Some("oauth")) => None,
            _ => Some(provider_label(config, provider, kind)),
        },
        metered: if kind == Some("oauth") {
            Some(true)
        } else if kind.is_some_and(|kind| config.api_key_types.contains(&kind)) {
            Some(false)
        } else {
            None
        },
        sub_provider: Some(provider.clone()),
        credentials_updated_at_ms: loaded.stamp,
        ..Default::default()
    })
}

pub(super) fn probe_account_usage(config: &Config) -> AccountUsageProbe {
    let loaded = (config.load_credentials)();
    let stamp = loaded.stamp;
    let selected = loaded
        .entries
        .and_then(|entries| select_usage_entries(&entries, config));
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
    let result = match quota_surface(config.adapter, &selected.provider) {
        Some(QuotaSurface::Anthropic) => {
            super::adapters::claude::oauth_usage::fetch_usage_with_token(&selected.access_token)
                .map_err(|source| Error::Claude {
                    adapter: config.adapter.name(),
                    source,
                })
        }
        Some(QuotaSurface::Codex) => super::adapters::codex::oauth_usage::fetch_usage_with_token(
            &selected.access_token,
            selected.account_id.as_deref(),
        )
        .map_err(|source| Error::Codex {
            adapter: config.adapter.name(),
            source,
        }),
        Some(QuotaSurface::Go) => super::adapters::opencode::go_usage::fetch_usage_with_token(
            &selected.access_token,
            selected.org_id.as_deref(),
        )
        .map_err(|source| Error::Go {
            adapter: config.adapter.name(),
            source,
        }),
        None => Err(Error::UnsupportedProvider {
            adapter: config.adapter.name(),
            provider: selected.provider,
        }),
    };
    super::credits::map_account_usage_probe(result, identity, config.adapter.name())
}

fn select_display<'a>(
    credentials: &'a BTreeMap<String, Credential>,
    used_provider: Option<&str>,
    adapter: Adapter,
) -> Option<(&'a String, &'a Credential)> {
    used_credential(credentials, used_provider, adapter)
        .or_else(|| {
            credentials
                .iter()
                .find(|(_, credential)| credential.kind.as_deref() == Some("oauth"))
        })
        .or_else(|| credentials.iter().next())
}

fn used_credential<'a>(
    credentials: &'a BTreeMap<String, Credential>,
    used_provider: Option<&str>,
    adapter: Adapter,
) -> Option<(&'a String, &'a Credential)> {
    let provider = used_provider?;
    let provider = if adapter == Adapter::OpenCode
        && provider == "opencode-go"
        && !credentials.contains_key(provider)
    {
        "opencode"
    } else {
        provider
    };
    credentials.get_key_value(provider)
}

pub(super) fn load_auth_file(path: &Path, adapter: Adapter) -> Credentials {
    let entries = std::fs::read(path)
        .map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                Error::NoCredentials {
                    adapter: adapter.name(),
                }
            } else {
                Error::Io {
                    adapter: adapter.name(),
                    source,
                }
            }
        })
        .and_then(|bytes| {
            serde_json::from_slice(&bytes).map_err(|source| Error::Parse {
                adapter: adapter.name(),
                source,
            })
        });
    Credentials {
        stamp: file_mtime_ms(path),
        entries,
    }
}

#[cfg(test)]
fn select_usage(bytes: &[u8], config: &Config) -> Result<SelectedCredential, Error> {
    let credentials: BTreeMap<String, Credential> =
        serde_json::from_slice(bytes).map_err(|source| Error::Parse {
            adapter: config.adapter.name(),
            source,
        })?;
    select_usage_entries(&credentials, config)
}

fn select_usage_entries(
    credentials: &BTreeMap<String, Credential>,
    config: &Config,
) -> Result<SelectedCredential, Error> {
    if credentials.is_empty() {
        return Err(Error::NoCredentials {
            adapter: config.adapter.name(),
        });
    }
    let used_provider = (config.used_provider)();
    let Some((provider, credential)) =
        used_credential(credentials, used_provider.as_deref(), config.adapter).or_else(|| {
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
    let account_id = credential
        .account_id
        .as_ref()
        .or(credential.metadata.account_id.as_ref())
        .filter(|id| !id.is_empty())
        .cloned();
    let org_id = credential
        .metadata
        .org_id
        .clone()
        .filter(|id| !id.is_empty());
    let refresh_token = credential
        .refresh
        .as_deref()
        .filter(|token| !token.is_empty());
    let scope = oauth_scope(config.adapter, provider).unwrap_or_default();
    let fallback = hashed_account_key(
        config.account_key_domain,
        refresh_token
            .as_ref()
            .map_or("access-token", |_| "refresh-token"),
        refresh_token.unwrap_or(&access_token),
    );
    let account_key = if scope == ProviderAccountScope::sub_provider("openai", "oauth") {
        account_id.clone().unwrap_or(fallback)
    } else if scope == ProviderAccountScope::sub_provider("opencode", "oauth")
        && let Some(org_id) = &org_id
    {
        hashed_account_key(
            config.account_key_domain,
            "console-org",
            &format!("{fallback}\0{org_id}"),
        )
    } else {
        fallback
    };
    Ok(SelectedCredential {
        provider: provider.clone(),
        access_token,
        account_key,
        scope,
        account_id,
        org_id,
    })
}

enum QuotaSurface {
    Anthropic,
    Codex,
    Go,
}

fn quota_surface(adapter: Adapter, provider: &str) -> Option<QuotaSurface> {
    match (adapter, provider) {
        (_, "anthropic") => Some(QuotaSurface::Anthropic),
        (_, "openai-codex") | (Adapter::OpenCode, "openai") => Some(QuotaSurface::Codex),
        (Adapter::OpenCode, "opencode") => Some(QuotaSurface::Go),
        _ => None,
    }
}

/// Whether this login has an out-of-band quota reading RimZ can expect.
pub fn expects_usage_reading(
    kind: &crate::ids::AgentKind,
    metered: Option<bool>,
    sub_provider: Option<&str>,
) -> bool {
    if metered == Some(false) {
        return false;
    }
    let Ok(definition) = super::registry::definition_by_kind(kind.as_str()) else {
        return false;
    };
    let spec = definition.spec();
    if !spec.expected_windows.is_empty() {
        return true;
    }
    if !spec.capabilities.direct_account_usage || metered != Some(true) {
        return false;
    }
    sub_provider.is_none_or(|provider| definition.has_quota_surface(provider))
}

fn oauth_scope(adapter: Adapter, provider: &str) -> Option<ProviderAccountScope> {
    quota_surface(adapter, provider).map(|surface| match surface {
        QuotaSurface::Anthropic => ProviderAccountScope::sub_provider("anthropic", "oauth"),
        QuotaSurface::Codex => ProviderAccountScope::sub_provider("openai", "oauth"),
        QuotaSurface::Go => ProviderAccountScope::sub_provider("opencode", "oauth"),
    })
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
        Some("external") if config.adapter == Adapter::OpenCode => format!("{name} External"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn delegated_readings_require_a_metered_quota_surface() {
        for (kind, provider, expected) in [
            ("pi", "openai", false),
            ("pi", "openai-codex", true),
            ("pi", "anthropic", true),
            ("opencode", "openai", true),
            ("opencode", "anthropic", true),
            ("opencode", "other", false),
        ] {
            let kind = crate::ids::AgentKind::new_unchecked(kind);
            assert_eq!(
                expects_usage_reading(&kind, Some(true), Some(provider)),
                expected,
                "{kind}/{provider}"
            );
            assert!(!expects_usage_reading(&kind, Some(false), Some(provider)));
            assert!(!expects_usage_reading(&kind, None, Some(provider)));
        }
    }

    #[test]
    fn undeclared_sub_provider_quota_surfaces_do_not_expect_readings() {
        let kind = crate::ids::AgentKind::new_unchecked("copilot");
        assert!(expects_usage_reading(&kind, Some(true), None));
        assert!(!expects_usage_reading(&kind, Some(true), Some("openai")));
    }

    #[test]
    fn reading_expectations_keep_static_windows_and_direct_metered_accounts() {
        for kind in ["claude", "codex", "kimi"] {
            let kind = crate::ids::AgentKind::new_unchecked(kind);
            assert!(expects_usage_reading(&kind, None, None), "{kind}");
            assert!(!expects_usage_reading(&kind, Some(false), None), "{kind}");
        }
        for (kind, expected) in [
            ("copilot", true),
            ("qwen", true),
            ("antigravity", true),
            ("grok", false),
            ("cursor", false),
            ("amp", false),
        ] {
            let kind = crate::ids::AgentKind::new_unchecked(kind);
            assert_eq!(
                expects_usage_reading(&kind, Some(true), None),
                expected,
                "{kind}"
            );
            assert!(!expects_usage_reading(&kind, None, None), "{kind}");
        }
        let qwen = crate::ids::AgentKind::new_unchecked("qwen");
        for provider in [
            "Alibaba Coding Plan (International)",
            "Alibaba Coding Plan (China)",
        ] {
            assert!(expects_usage_reading(&qwen, Some(true), Some(provider)));
        }
    }

    fn no_used_provider() -> Option<String> {
        None
    }

    fn openai_used_provider() -> Option<String> {
        Some("openai".to_owned())
    }

    fn opencode_used_provider() -> Option<String> {
        Some("opencode".to_owned())
    }

    fn go_used_provider() -> Option<String> {
        Some("opencode-go".to_owned())
    }

    fn config(adapter: Adapter, used: Option<&str>, domain: &'static [u8]) -> Config {
        Config {
            adapter,
            load_credentials: Box::new(|| Credentials {
                stamp: None,
                entries: Ok(BTreeMap::new()),
            }),
            used_provider: Box::new(match used {
                Some("openai") => openai_used_provider,
                Some("opencode") => opencode_used_provider,
                Some("opencode-go") => go_used_provider,
                Some(other) => panic!("missing test resolver for {other}"),
                None => no_used_provider,
            }),
            api_key_types: match adapter {
                Adapter::Pi => &["api_key"],
                Adapter::OpenCode => &["key"],
            },
            account_key_domain: domain,
        }
    }

    fn future_ms() -> u64 {
        unix_now_ms() + 60_000
    }

    #[test]
    fn go_sessions_link_console_credentials_but_exact_credentials_win() {
        let outcomes = [None, Some("oauth"), Some("key")].map(|exact_kind| {
            let mut body = serde_json::json!({
                "anthropic": {"type":"oauth", "access":"a", "expires":future_ms()},
                "opencode": {"type":"oauth", "access":"c", "expires":future_ms()}
            });
            if let Some(kind) = exact_kind {
                body["opencode-go"] = serde_json::json!({
                    "type":kind, "access":"exact", "expires":future_ms()
                });
            }
            let mut config = config(Adapter::OpenCode, Some("opencode-go"), b"open");
            let display_body = body.clone();
            config.load_credentials = Box::new(move || Credentials {
                stamp: Some(42),
                entries: Ok(serde_json::from_value(display_body.clone()).unwrap()),
            });
            let AccountProbe::Found(account) = probe_account(&config) else {
                panic!("Go session should select a stored credential")
            };
            let usage = select_usage(body.to_string().as_bytes(), &config)
                .map(|selected| selected.provider)
                .map_err(|error| matches!(error, Error::ApiKeyOnly { .. }));
            (account.sub_provider, usage)
        });
        assert_eq!(
            outcomes,
            [
                (Some("opencode".to_owned()), Ok("opencode".to_owned())),
                (Some("opencode-go".to_owned()), Ok("opencode-go".to_owned())),
                (Some("opencode-go".to_owned()), Err(true)),
            ]
        );
    }

    #[test]
    fn console_oauth_has_display_only_scope_and_leaves_the_plan_to_usage() {
        let surfaces =
            [Adapter::OpenCode, Adapter::Pi].map(|adapter| adapter.has_quota_surface("opencode"));
        assert_eq!(surfaces, [true, false]);
        let mut config = config(Adapter::OpenCode, Some("opencode"), b"open");
        config.load_credentials = Box::new(|| Credentials {
            stamp: Some(42),
            entries: Ok(serde_json::from_str(r#"{"opencode":{"type":"oauth"}}"#).unwrap()),
        });
        let AccountProbe::Found(account) = probe_account(&config) else {
            panic!("Console credential should select an account")
        };
        assert_eq!(
            account.scope,
            ProviderAccountScope::sub_provider("opencode", "oauth")
        );
        assert_eq!(account.plan, None);
        assert_eq!(account.metered, Some(true));
        let kind = crate::ids::AgentKind::new_unchecked("opencode");
        assert!(expects_usage_reading(&kind, Some(true), Some("opencode")));
        assert!(!expects_usage_reading(&kind, Some(false), Some("opencode")));
    }

    #[test]
    fn console_org_changes_account_identity_without_exposing_it() {
        let keys = [
            None,
            Some(""),
            Some("private-org-one"),
            Some("private-org-two"),
        ]
        .map(|org| {
            let body = serde_json::json!({"opencode": {
                "type":"oauth", "access":"same-token", "refresh":"stable-refresh",
                "expires":future_ms(), "metadata":{"orgID":org}
            }});
            let selected = select_usage(
                body.to_string().as_bytes(),
                &config(Adapter::OpenCode, Some("opencode"), b"open"),
            )
            .unwrap();
            selected.account_key
        });
        assert_eq!(keys[0], keys[1]);
        assert_ne!(
            keys[2], keys[3],
            "a Console org switch changes the cache owner"
        );
        for key in keys {
            assert_eq!(key.len(), 64);
            assert!(!key.contains("private-org"));
            assert!(!key.contains("same-token"));
        }
    }

    #[test]
    fn pi_opencode_display_selection_preserves_priority_labels_and_metering() {
        let credentials: BTreeMap<String, Credential> = serde_json::from_str(
            r#"{
                "anthropic":{"type":"oauth"},
                "openai":{"type":"key"},
                "opencode":{"type":"external"}
            }"#,
        )
        .unwrap();
        let selected = select_display(&credentials, Some("openai"), Adapter::OpenCode).unwrap();
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
                Some("external")
            ),
            "OpenCode External"
        );
        assert_eq!(
            provider_label(&config(Adapter::Pi, None, b"pi"), "custom", None),
            "custom"
        );
        assert_eq!(
            provider_label(&config(Adapter::Pi, None, b"pi"), "opencode", None),
            "opencode"
        );
        assert_eq!(
            select_display(&credentials, None, Adapter::OpenCode)
                .unwrap()
                .0,
            "anthropic"
        );
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
        assert_eq!(
            pi.account_key,
            hashed_account_key(b"pi", "access-token", "o")
        );
        assert_eq!(pi.scope, ProviderAccountScope::default());
        let opencode = select_usage(
            json.as_bytes(),
            &config(Adapter::OpenCode, Some("openai"), b"open"),
        )
        .unwrap();
        assert_eq!(opencode.account_key, "acct");
        assert_eq!(
            opencode.scope,
            ProviderAccountScope::sub_provider("openai", "oauth")
        );
        let codex = format!(
            r#"{{"openai-codex":{{"type":"oauth","access":"c","expires":{},"accountId":"codex-acct"}}}}"#,
            future_ms()
        );
        let codex = select_usage(codex.as_bytes(), &config(Adapter::Pi, None, b"pi")).unwrap();
        assert_eq!(codex.account_key, "codex-acct");
        assert_eq!(
            codex.scope,
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
        let auth_path = path.clone();
        pi.load_credentials = Box::new(move || load_auth_file(&auth_path, Adapter::Pi));
        assert!(matches!(probe_account(&pi), AccountProbe::LoggedOut));

        std::fs::write(&path, b"not json").unwrap();
        assert!(matches!(
            probe_account(&pi),
            AccountProbe::Unavailable(AccountProbeError::UnparseableOutput)
        ));

        std::fs::write(
            &path,
            br#"{
                "anthropic":{"type":"oauth"},
                "openai":{"type":"api_key"},
                "opencode":{"type":"external"}
            }"#,
        )
        .unwrap();
        let AccountProbe::Found(account) = probe_account(&pi) else {
            panic!("pi account")
        };
        assert_eq!(account.plan.as_deref(), Some("OpenAI API Key"));
        assert_eq!(account.metered, Some(false));
        assert!(account.credentials_updated_at_ms.is_some());

        std::fs::write(
            &path,
            format!(
                r#"{{"openai":{{"type":"oauth","access":"o","expires":{},"accountId":"acct"}}}}"#,
                future_ms()
            ),
        )
        .unwrap();
        let AccountProbe::Found(account) = probe_account(&pi) else {
            panic!("pi OAuth account")
        };
        assert_eq!(account.plan.as_deref(), Some("OpenAI OAuth"));
        assert_eq!(account.metered, Some(true));
        assert_eq!(account.scope, ProviderAccountScope::default());

        std::fs::write(
            &path,
            br#"{
                "anthropic":{"type":"oauth"},
                "openai":{"type":"api_key"},
                "opencode":{"type":"external"}
            }"#,
        )
        .unwrap();

        let mut opencode = config(Adapter::OpenCode, Some("opencode"), b"open");
        opencode.load_credentials = Box::new(move || load_auth_file(&path, Adapter::OpenCode));
        let AccountProbe::Found(account) = probe_account(&opencode) else {
            panic!("opencode account")
        };
        assert_eq!(account.plan.as_deref(), Some("OpenCode External"));
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
        let auth_path = path.clone();
        config.load_credentials = Box::new(move || load_auth_file(&auth_path, Adapter::OpenCode));
        config.used_provider = Box::new(counted_openai_provider);

        assert!(matches!(probe_account(&config), AccountProbe::LoggedOut));
        std::fs::write(&path, b"not json").unwrap();
        assert!(matches!(
            probe_account(&config),
            AccountProbe::Unavailable(AccountProbeError::UnparseableOutput)
        ));
        std::fs::write(&path, b"{}").unwrap();
        assert!(matches!(probe_account(&config), AccountProbe::LoggedOut));
        assert_eq!(RESOLVER_CALLS.load(Ordering::Relaxed), 0);

        std::fs::write(
            &path,
            br#"{"anthropic":{"type":"oauth"},"openai":{"type":"key"}}"#,
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
