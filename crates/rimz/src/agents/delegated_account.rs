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
    pub(crate) used_provider: Box<dyn Fn() -> UsedProvider>,
    pub(crate) api_key_types: &'static [&'static str],
    pub(crate) account_key_domain: &'static [u8],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum UsedProvider {
    Named(String),
    Fallback(String),
    Silent,
}

impl UsedProvider {
    pub(super) fn provider(&self) -> Option<&str> {
        match self {
            Self::Named(provider) | Self::Fallback(provider) => Some(provider),
            Self::Silent => None,
        }
    }
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
    let mut account = match select_display(&credentials, used_provider.provider(), config.adapter) {
        Some((provider, credential)) => credential_account(config, provider, credential),
        None => AgentAccount {
            sub_provider: used_provider.provider().map(str::to_owned),
            metered: used_provider.provider().map(|_| false),
            ..Default::default()
        },
    };
    account.provisional = !matches!(used_provider, UsedProvider::Named(_)) && credentials.len() > 1;
    account.credentials_updated_at_ms = loaded.stamp;
    account.credentials = credentials
        .iter()
        .map(|(provider, credential)| {
            let mut account = credential_account(config, provider, credential);
            account.credentials_updated_at_ms = loaded.stamp;
            (provider.clone(), account)
        })
        .collect();
    AccountProbe::Found(account)
}

fn credential_account(config: &Config, provider: &str, credential: &Credential) -> AgentAccount {
    let kind = credential.kind.as_deref();
    let scope = (kind == Some("oauth"))
        .then(|| oauth_scope(config.adapter, provider))
        .flatten()
        .unwrap_or_default();
    AgentAccount {
        account_key: (kind == Some("oauth"))
            .then(|| credential_account_key(config, &scope, credential))
            .flatten(),
        scope,
        plan: match (config.adapter, provider, kind) {
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
        sub_provider: Some(provider.to_owned()),
        ..Default::default()
    }
}

pub(super) fn probe_account_usage(
    config: &Config,
    sub_provider: Option<&str>,
) -> AccountUsageProbe {
    let loaded = (config.load_credentials)();
    let stamp = loaded.stamp;
    let selected = loaded
        .entries
        .and_then(|entries| select_usage_entries(&entries, config, sub_provider));
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
    if used_provider.is_some() {
        return used_credential(credentials, used_provider, adapter);
    }
    (credentials.len() == 1)
        .then(|| credentials.iter().next())
        .flatten()
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
    select_usage_entries(&credentials, config, None)
}

fn select_usage_entries(
    credentials: &BTreeMap<String, Credential>,
    config: &Config,
    sub_provider: Option<&str>,
) -> Result<SelectedCredential, Error> {
    if credentials.is_empty() {
        return Err(Error::NoCredentials {
            adapter: config.adapter.name(),
        });
    }
    let used_provider = sub_provider
        .map(|provider| UsedProvider::Named(provider.to_owned()))
        .unwrap_or_else(|| (config.used_provider)());
    let Some((provider, credential)) =
        select_display(credentials, used_provider.provider(), config.adapter)
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
    let scope = oauth_scope(config.adapter, provider).unwrap_or_default();
    let account_key =
        credential_account_key(config, &scope, credential).ok_or(Error::NoCredentials {
            adapter: config.adapter.name(),
        })?;
    Ok(SelectedCredential {
        provider: provider.clone(),
        access_token,
        account_key,
        scope,
        account_id,
        org_id,
    })
}

fn credential_account_key(
    config: &Config,
    scope: &ProviderAccountScope,
    credential: &Credential,
) -> Option<String> {
    let refresh_token = credential
        .refresh
        .as_deref()
        .filter(|token| !token.is_empty());
    let token = refresh_token.or_else(|| {
        credential
            .access
            .as_deref()
            .filter(|token| !token.is_empty())
    })?;
    let fallback = hashed_account_key(
        config.account_key_domain,
        refresh_token
            .as_ref()
            .map_or("access-token", |_| "refresh-token"),
        token,
    );
    if *scope == ProviderAccountScope::sub_provider("openai", "oauth") {
        Some(
            credential
                .account_id
                .as_ref()
                .or(credential.metadata.account_id.as_ref())
                .filter(|id| !id.is_empty())
                .cloned()
                .unwrap_or(fallback),
        )
    } else if *scope == ProviderAccountScope::sub_provider("opencode", "oauth")
        && let Some(org_id) = credential
            .metadata
            .org_id
            .as_ref()
            .filter(|id| !id.is_empty())
    {
        Some(hashed_account_key(
            config.account_key_domain,
            "console-org",
            &format!("{fallback}\0{org_id}"),
        ))
    } else {
        Some(fallback)
    }
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
mod tests;
