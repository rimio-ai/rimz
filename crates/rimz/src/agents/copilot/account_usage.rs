//! Bounded Copilot account-plan and monthly-quota enrichment.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Deserializer};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::agents::account::file_mtime_ms;
use crate::agents::context::{
    AgentRateLimits, RateLimitWindow, RateLimitWindowScope, WindowSource, clamp_pct,
};
use crate::agents::credits::oauth_http_get;
use crate::agents::{AccountUsageIdentity, AccountUsageSnapshot, HttpErrKind};

const ACCOUNT_KEY_DOMAIN: &[u8] = b"rimz/copilot-account-key/v1";
const USAGE_PATH: &str = "/copilot_internal/user";
const API_VERSION: &str = "2025-04-01";
const EDITOR_VERSION: &str = "vscode/1.96.2";
const PLUGIN_VERSION: &str = "copilot-chat/0.26.7";
const USER_AGENT: &str = "GitHubCopilotChat/0.26.7";

#[derive(Debug, thiserror::Error)]
pub(crate) enum CopilotUsageErr {
    #[error("Copilot account-usage credentials are unavailable")]
    NoCredentials,
    #[error("Copilot credential state is unavailable")]
    ConfigUnavailable,
    #[error("Copilot GitHub host is invalid")]
    InvalidHost,
    #[error("Copilot account-usage HTTP {kind} (host {host})")]
    Http { kind: HttpErrKind, host: String },
    #[error("parsing Copilot account-usage response: {0}")]
    Response(#[from] serde_json::Error),
    #[error("Copilot account-usage response has no usable plan or quota")]
    UnusableSchema,
}

impl crate::agents::credits::AccountUsageReportable for CopilotUsageErr {
    fn should_report(&self) -> bool {
        !matches!(
            self,
            Self::NoCredentials | Self::ConfigUnavailable | Self::InvalidHost
        ) && !matches!(
            self,
            Self::Http { kind, .. } if kind.is_auth_rejected()
        )
    }
}

type Result<T> = std::result::Result<T, CopilotUsageErr>;

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct UsageConfig {
    last_logged_in_user: Option<LoginIdentity>,
    logged_in_users: Vec<LoginIdentity>,
    copilot_tokens: BTreeMap<String, Value>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct LoginIdentity {
    host: Option<String>,
    login: Option<String>,
}

struct ActiveIdentity {
    host: String,
    login: String,
}

#[derive(Default)]
struct CredentialEnv {
    copilot_token: Option<String>,
    gh_token: Option<String>,
    github_token: Option<String>,
    copilot_host: Option<String>,
    gh_host: Option<String>,
}

impl CredentialEnv {
    fn from_process() -> Self {
        Self {
            copilot_token: env_value("COPILOT_GITHUB_TOKEN"),
            gh_token: env_value("GH_TOKEN"),
            github_token: env_value("GITHUB_TOKEN"),
            copilot_host: env_value("COPILOT_GH_HOST"),
            gh_host: env_value("GH_HOST"),
        }
    }

    fn token(&self) -> Option<&str> {
        self.copilot_token
            .as_deref()
            .or(self.gh_token.as_deref())
            .or(self.github_token.as_deref())
    }

    fn host(&self) -> Option<&str> {
        self.copilot_host.as_deref().or(self.gh_host.as_deref())
    }
}

struct SelectedCredential {
    host: String,
    token: String,
    identity: AccountUsageIdentity,
}

pub(super) fn probe_usage() -> crate::agents::AccountUsageProbe {
    let (config, credentials_stamp) = match process_config() {
        Ok(config) => config,
        Err(err) => {
            return crate::agents::credits::map_account_usage_probe(
                Err(err),
                AccountUsageIdentity::default(),
                "copilot",
            );
        }
    };
    let selected = match select_credential(
        config.as_ref(),
        &CredentialEnv::from_process(),
        credentials_stamp,
    ) {
        Ok(selected) => selected,
        Err(err) => {
            return crate::agents::credits::map_account_usage_probe(
                Err(err),
                AccountUsageIdentity {
                    credentials_stamp,
                    ..Default::default()
                },
                "copilot",
            );
        }
    };
    let identity = selected.identity.clone();
    crate::agents::credits::map_account_usage_probe(
        fetch_usage_with_url(&usage_url(&selected.host), &selected.token),
        identity,
        "copilot",
    )
}

fn process_config() -> Result<(Option<UsageConfig>, Option<u64>)> {
    let path = config_path();
    let credentials_stamp = path.as_deref().and_then(file_mtime_ms);
    let config = path.as_deref().map(load_config).transpose()?.flatten();
    Ok((config, credentials_stamp))
}

fn config_path() -> Option<PathBuf> {
    Some(super::paths::copilot_home()?.join("config.json"))
}

fn load_config(path: &Path) -> Result<Option<UsageConfig>> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(CopilotUsageErr::ConfigUnavailable),
    };
    crate::agents::jsonc::from_slice(&bytes)
        .map(Some)
        .map_err(|_| CopilotUsageErr::ConfigUnavailable)
}

fn select_credential(
    config: Option<&UsageConfig>,
    env: &CredentialEnv,
    config_stamp: Option<u64>,
) -> Result<SelectedCredential> {
    let active = config.and_then(UsageConfig::active_identity);
    let host = match env.host() {
        Some(host) => super::account::normalized_host(host).ok_or(CopilotUsageErr::InvalidHost)?,
        None => active
            .as_ref()
            .map(|identity| identity.host.clone())
            .unwrap_or_else(|| "github.com".to_owned()),
    };
    let (token, credentials_stamp) = match env.token() {
        Some(token) => (token.to_owned(), None),
        None => {
            let active = active.as_ref().ok_or(CopilotUsageErr::NoCredentials)?;
            if active.host != host {
                return Err(CopilotUsageErr::NoCredentials);
            }
            let token = config
                .and_then(|config| config.token_for(active))
                .ok_or(CopilotUsageErr::NoCredentials)?;
            (token.to_owned(), config_stamp)
        }
    };
    let identity = AccountUsageIdentity {
        account_key: Some(account_key(&host, &token)),
        credentials_stamp,
        ..Default::default()
    };
    Ok(SelectedCredential {
        host,
        token,
        identity,
    })
}

impl UsageConfig {
    fn active_identity(&self) -> Option<ActiveIdentity> {
        self.last_logged_in_user
            .iter()
            .chain(&self.logged_in_users)
            .find_map(LoginIdentity::normalized)
    }

    fn token_for(&self, identity: &ActiveIdentity) -> Option<&str> {
        self.copilot_tokens.iter().find_map(|(key, value)| {
            let candidate = token_key_identity(key)?;
            (candidate.host == identity.host && candidate.login == identity.login)
                .then(|| value.as_str().map(str::trim))
                .flatten()
                .filter(|token| !token.is_empty())
        })
    }
}

impl LoginIdentity {
    fn normalized(&self) -> Option<ActiveIdentity> {
        let login = self.login.as_deref()?.trim();
        if login.is_empty() {
            return None;
        }
        let host = self
            .host
            .as_deref()
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .unwrap_or("github.com");
        Some(ActiveIdentity {
            host: super::account::normalized_host(host)?,
            login: login.to_owned(),
        })
    }
}

fn token_key_identity(key: &str) -> Option<ActiveIdentity> {
    let (host, login) = key.rsplit_once(':')?;
    let login = login.trim();
    if login.is_empty() {
        return None;
    }
    Some(ActiveIdentity {
        host: super::account::normalized_host(host)?,
        login: login.to_owned(),
    })
}

fn env_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

fn account_key(host: &str, token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(ACCOUNT_KEY_DOMAIN);
    hasher.update([0]);
    hasher.update(host.as_bytes());
    hasher.update([0]);
    hasher.update(token.as_bytes());
    hex::encode(hasher.finalize())
}

fn api_host(host: &str) -> String {
    let (name, port) = host
        .rsplit_once(':')
        .filter(|(name, _)| !name.contains(':'))
        .map_or((host, None), |(name, port)| (name, Some(port)));
    let api_name = if name == "github.com" {
        "api.github.com".to_owned()
    } else if name.starts_with("api.") {
        name.to_owned()
    } else {
        format!("api.{name}")
    };
    port.map_or(api_name.clone(), |port| format!("{api_name}:{port}"))
}

fn usage_url(host: &str) -> String {
    format!("https://{}{USAGE_PATH}", api_host(host))
}

fn fetch_usage_with_url(url: &str, token: &str) -> Result<AccountUsageSnapshot> {
    let headers = [
        ("Authorization", format!("token {token}")),
        ("Accept", "application/vnd.github+json".to_owned()),
        ("X-GitHub-Api-Version", API_VERSION.to_owned()),
        ("Editor-Version", EDITOR_VERSION.to_owned()),
        ("Editor-Plugin-Version", PLUGIN_VERSION.to_owned()),
        ("User-Agent", USER_AGENT.to_owned()),
    ];
    let body = oauth_http_get(url, &headers, "copilot: fetching account usage")
        .map_err(|(kind, host)| CopilotUsageErr::Http { kind, host })?;
    parse_usage_response(&body)
}

fn parse_usage_response(body: &str) -> Result<AccountUsageSnapshot> {
    UsageWire::from_json(body)?.into_snapshot()
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct UsageWire {
    copilot_plan: Option<String>,
    token_based_billing: Option<bool>,
    quota_reset_date: Option<String>,
    limited_user_reset_date: Option<String>,
    quota_snapshots: QuotaSnapshots,
    monthly_quotas: LegacyQuotas,
    limited_user_quotas: LegacyQuotas,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct QuotaSnapshots {
    chat: Option<QuotaWire>,
    premium_interactions: Option<QuotaWire>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct QuotaWire {
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    entitlement: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    remaining: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    quota_remaining: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    percent_remaining: Option<f64>,
    unlimited: Option<bool>,
    #[serde(
        alias = "reset_date",
        alias = "reset_at",
        alias = "resets_at",
        alias = "quota_reset_date"
    )]
    reset: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct LegacyQuotas {
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    chat: Option<f64>,
    #[serde(default, deserialize_with = "deserialize_optional_number")]
    premium_interactions: Option<f64>,
}

impl UsageWire {
    fn from_json(body: &str) -> Result<Self> {
        Ok(serde_json::from_str(body)?)
    }

    fn into_snapshot(self) -> Result<AccountUsageSnapshot> {
        let plan = self
            .copilot_plan
            .map(|plan| plan.trim().to_owned())
            .filter(|plan| !plan.is_empty());
        let quota_reset = self.quota_reset_date.as_deref().and_then(parse_reset);
        let legacy_reset = self
            .limited_user_reset_date
            .as_deref()
            .and_then(parse_reset)
            .or(quota_reset);
        let chat_label = if self.token_based_billing == Some(true) {
            "cr"
        } else {
            "cht"
        };
        let chat = self
            .quota_snapshots
            .chat
            .and_then(|quota| quota.into_window("chat", chat_label, quota_reset))
            .or_else(|| {
                legacy_window(
                    self.monthly_quotas.chat,
                    self.limited_user_quotas.chat,
                    "chat",
                    chat_label,
                    legacy_reset,
                )
            });
        let premium = self
            .quota_snapshots
            .premium_interactions
            .and_then(|quota| quota.into_window("premium_interactions", "prm", quota_reset))
            .or_else(|| {
                legacy_window(
                    self.monthly_quotas.premium_interactions,
                    self.limited_user_quotas.premium_interactions,
                    "premium_interactions",
                    "prm",
                    legacy_reset,
                )
            });
        let windows: Vec<_> = [chat, premium].into_iter().flatten().collect();
        if windows.is_empty() && plan.is_none() {
            return Err(CopilotUsageErr::UnusableSchema);
        }
        Ok(AccountUsageSnapshot {
            rate_limits: (!windows.is_empty()).then_some(AgentRateLimits { windows }),
            plan,
            ..Default::default()
        })
    }
}

impl QuotaWire {
    fn into_window(
        self,
        scope_id: &str,
        label: &str,
        fallback_reset: Option<Timestamp>,
    ) -> Option<RateLimitWindow> {
        if self.unlimited == Some(true) {
            return Some(named_window(scope_id, label, None, None, true));
        }
        if self
            .entitlement
            .is_some_and(|entitlement| entitlement <= 0.0)
        {
            return None;
        }
        let remaining_percentage = self.percent_remaining.or_else(|| {
            let entitlement = self.entitlement.filter(|entitlement| *entitlement > 0.0)?;
            let remaining = self.remaining.or(self.quota_remaining)?;
            Some(remaining * 100.0 / entitlement)
        })?;
        let used_percentage = clamp_pct(Some(100.0 - remaining_percentage))?;
        let resets_at = self
            .reset
            .as_deref()
            .and_then(parse_reset)
            .or(fallback_reset);
        Some(named_window(
            scope_id,
            label,
            Some(used_percentage),
            resets_at,
            false,
        ))
    }
}

fn legacy_window(
    entitlement: Option<f64>,
    remaining: Option<f64>,
    scope_id: &str,
    label: &str,
    resets_at: Option<Timestamp>,
) -> Option<RateLimitWindow> {
    let entitlement = entitlement.filter(|entitlement| *entitlement > 0.0)?;
    let used_percentage = clamp_pct(Some(100.0 - remaining? * 100.0 / entitlement))?;
    Some(named_window(
        scope_id,
        label,
        Some(used_percentage),
        resets_at,
        false,
    ))
}

fn named_window(
    scope_id: &str,
    label: &str,
    used_percentage: Option<u8>,
    resets_at: Option<Timestamp>,
    lifted: bool,
) -> RateLimitWindow {
    RateLimitWindow {
        scope: Some(RateLimitWindowScope {
            id: scope_id.to_owned(),
            label: label.to_owned(),
        }),
        used_percentage,
        resets_at,
        source: WindowSource::Authoritative,
        lifted,
        ..Default::default()
    }
}

fn parse_reset(raw: &str) -> Option<Timestamp> {
    let raw = raw.trim();
    raw.parse::<Timestamp>().ok().or_else(|| {
        raw.parse::<jiff::civil::Date>().ok()?;
        format!("{raw}T00:00:00Z").parse::<Timestamp>().ok()
    })
}

fn deserialize_optional_number<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<f64>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    Ok(value.and_then(|value| {
        let number = match value {
            Value::Number(number) => number.as_f64(),
            Value::String(raw) => raw.trim().parse::<f64>().ok(),
            _ => None,
        }?;
        number.is_finite().then_some(number)
    }))
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    fn config(body: &str) -> UsageConfig {
        crate::agents::jsonc::from_slice(body.as_bytes()).unwrap()
    }

    fn env_with_token(token: &str) -> CredentialEnv {
        CredentialEnv {
            copilot_token: Some(token.to_owned()),
            ..Default::default()
        }
    }

    fn snapshot_windows(body: &str) -> (AccountUsageSnapshot, Vec<RateLimitWindow>) {
        let snapshot = parse_usage_response(body).unwrap();
        let windows = snapshot
            .rate_limits
            .clone()
            .map(|limits| limits.windows)
            .unwrap_or_default();
        (snapshot, windows)
    }

    fn window<'a>(windows: &'a [RateLimitWindow], id: &str) -> &'a RateLimitWindow {
        windows
            .iter()
            .find(|window| window.scope.as_ref().is_some_and(|scope| scope.id == id))
            .unwrap()
    }

    #[test]
    fn credential_and_host_precedence_follow_copilot_native_order() {
        let config = config(
            r#"{
                "lastLoggedInUser":{"host":"https://github.example:8443","login":"octocat"},
                "copilotTokens":{"https://github.example:8443:octocat":"config-token"}
            }"#,
        );
        let env = CredentialEnv {
            copilot_token: Some("copilot-token".to_owned()),
            gh_token: Some("gh-token".to_owned()),
            github_token: Some("github-token".to_owned()),
            copilot_host: Some("https://api.enterprise.test:9443/path".to_owned()),
            gh_host: Some("ignored.test".to_owned()),
        };
        let selected = select_credential(Some(&config), &env, Some(7)).unwrap();
        assert_eq!(selected.token, "copilot-token");
        assert_eq!(selected.host, "api.enterprise.test:9443");
        assert_eq!(selected.identity.credentials_stamp, None);

        let selected = select_credential(
            Some(&config),
            &CredentialEnv {
                gh_token: Some("gh-token".to_owned()),
                gh_host: Some("gh.example".to_owned()),
                ..Default::default()
            },
            Some(7),
        )
        .unwrap();
        assert_eq!(
            (selected.token.as_str(), selected.host.as_str()),
            ("gh-token", "gh.example")
        );

        let selected =
            select_credential(Some(&config), &CredentialEnv::default(), Some(7)).unwrap();
        assert_eq!(
            (selected.token.as_str(), selected.host.as_str()),
            ("config-token", "github.example:8443")
        );
        assert_eq!(selected.identity.credentials_stamp, Some(7));
    }

    #[test]
    fn config_tokens_require_the_exact_active_identity() {
        let mismatched = config(
            r#"{
                "lastLoggedInUser":{"host":"github.com","login":"bob"},
                "loggedInUsers":[{"host":"github.com","login":"alice"}],
                "copilotTokens":{"https://github.com:alice":"alice-secret"}
            }"#,
        );
        assert!(matches!(
            select_credential(Some(&mismatched), &CredentialEnv::default(), Some(1)),
            Err(CopilotUsageErr::NoCredentials)
        ));

        let matching = config(
            r#"{
                "lastLoggedInUser":{"host":"https://GitHub.COM./path","login":"bob"},
                "copilotTokens":{"https://github.com:bob":"bob-secret"}
            }"#,
        );
        assert_eq!(
            select_credential(Some(&matching), &CredentialEnv::default(), Some(1))
                .unwrap()
                .token,
            "bob-secret"
        );
        assert!(matches!(
            select_credential(
                Some(&matching),
                &CredentialEnv {
                    gh_host: Some("enterprise.example".to_owned()),
                    ..Default::default()
                },
                Some(1),
            ),
            Err(CopilotUsageErr::NoCredentials)
        ));
    }

    #[test]
    fn account_identity_is_deterministic_versioned_and_secret_free() {
        let first = account_key("github.com", "super-secret-token");
        assert_eq!(first, account_key("github.com", "super-secret-token"));
        assert_ne!(first, account_key("github.example", "super-secret-token"));
        assert_ne!(first, account_key("github.com", "other-token"));
        assert_eq!(first.len(), 64);
        assert!(!first.contains("secret"));
        assert!(!format!("{:?}", CopilotUsageErr::NoCredentials).contains("super-secret-token"));
    }

    #[test]
    fn public_and_enterprise_hosts_build_safe_api_urls() {
        for (host, expected) in [
            ("github.com", "https://api.github.com/copilot_internal/user"),
            (
                "github.com:8443",
                "https://api.github.com:8443/copilot_internal/user",
            ),
            (
                "github.example",
                "https://api.github.example/copilot_internal/user",
            ),
            (
                "api.github.example:9443",
                "https://api.github.example:9443/copilot_internal/user",
            ),
        ] {
            assert_eq!(usage_url(host), expected, "{host}");
        }
    }

    #[test]
    fn modern_quotas_use_named_monthly_scopes_and_scope_resets() {
        let (snapshot, windows) = snapshot_windows(
            r#"{
                "copilot_plan":" individual ",
                "token_based_billing":true,
                "quota_reset_date":"2026-08-01",
                "quota_snapshots":{
                    "chat":{"entitlement":"200","remaining":"162","percent_remaining":"81","reset_at":"2026-07-31T12:00:00Z"},
                    "premium_interactions":{"entitlement":100,"remaining":75}
                }
            }"#,
        );
        assert_eq!(snapshot.plan.as_deref(), Some("individual"));
        let chat = window(&windows, "chat");
        assert_eq!(chat.scope.as_ref().unwrap().label, "cr");
        assert_eq!(chat.used_percentage, Some(19));
        assert_eq!(chat.duration_mins, None);
        assert_eq!(chat.resets_at, "2026-07-31T12:00:00Z".parse().ok());
        let premium = window(&windows, "premium_interactions");
        assert_eq!(premium.scope.as_ref().unwrap().label, "prm");
        assert_eq!(premium.used_percentage, Some(25));
        assert_eq!(premium.resets_at, "2026-08-01T00:00:00Z".parse().ok());
    }

    #[test]
    fn legacy_and_partial_modern_data_fill_only_missing_genuine_scopes() {
        let (_, windows) = snapshot_windows(
            r#"{
                "copilot_plan":"individual",
                "limited_user_reset_date":"2026-09-01",
                "quota_snapshots":{"chat":{"entitlement":200,"remaining":150}},
                "monthly_quotas":{"chat":500,"premium_interactions":"50","completions":2000},
                "limited_user_quotas":{"chat":100,"premium_interactions":"30","completions":1500}
            }"#,
        );
        assert_eq!(windows.len(), 2);
        assert_eq!(window(&windows, "chat").used_percentage, Some(25));
        assert_eq!(
            window(&windows, "premium_interactions").used_percentage,
            Some(40)
        );
        assert!(windows.iter().all(|window| {
            window
                .scope
                .as_ref()
                .is_none_or(|scope| scope.id != "completions")
        }));

        let (_, fallback) = snapshot_windows(
            r#"{
                "copilot_plan":"individual",
                "quota_snapshots":{"chat":{"remaining":30}},
                "monthly_quotas":{"chat":400},
                "limited_user_quotas":{"chat":100}
            }"#,
        );
        assert_eq!(window(&fallback, "chat").used_percentage, Some(75));
    }

    #[test]
    fn placeholders_unlimited_clamping_and_plan_only_are_tolerant() {
        let (snapshot, windows) = snapshot_windows(
            r#"{
                "copilot_plan":"business",
                "quota_snapshots":{
                    "chat":{"entitlement":0,"percent_remaining":100},
                    "premium_interactions":{"unlimited":true}
                }
            }"#,
        );
        assert_eq!(snapshot.plan.as_deref(), Some("business"));
        assert_eq!(windows.len(), 1);
        let premium = window(&windows, "premium_interactions");
        assert!(premium.lifted);
        assert_eq!(premium.used_percentage, None);
        assert_eq!(premium.resets_at, None);

        let (_, clamped) = snapshot_windows(
            r#"{
                "quota_snapshots":{
                    "chat":{"percent_remaining":"-20"},
                    "premium_interactions":{"percent_remaining":140}
                }
            }"#,
        );
        assert_eq!(window(&clamped, "chat").used_percentage, Some(100));
        assert_eq!(
            window(&clamped, "premium_interactions").used_percentage,
            Some(0)
        );

        let (plan_only, windows) =
            snapshot_windows(r#"{"copilot_plan":"business","token_based_billing":true}"#);
        assert_eq!(plan_only.plan.as_deref(), Some("business"));
        assert!(windows.is_empty());
        assert!(matches!(
            parse_usage_response(r#"{"quota_snapshots":{"chat":{"entitlement":"nan"}}}"#),
            Err(CopilotUsageErr::UnusableSchema)
        ));
    }

    #[test]
    fn request_uses_copilot_github_headers_without_leaking_into_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut bytes = [0_u8; 8192];
            let count = stream.read(&mut bytes).unwrap();
            let request = String::from_utf8_lossy(&bytes[..count]).into_owned();
            let body = r#"{"copilot_plan":"individual"}"#;
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            request
        });
        let url = format!("http://{address}{USAGE_PATH}");
        let snapshot = fetch_usage_with_url(&url, "test-secret").unwrap();
        assert_eq!(snapshot.plan.as_deref(), Some("individual"));
        let request = handle.join().unwrap().to_ascii_lowercase();
        for header in [
            "authorization: token test-secret",
            "accept: application/vnd.github+json",
            "x-github-api-version: 2025-04-01",
            "editor-version: vscode/1.96.2",
            "editor-plugin-version: copilot-chat/0.26.7",
            "user-agent: githubcopilotchat/0.26.7",
        ] {
            assert!(request.contains(header), "missing {header}: {request}");
        }

        let error = CopilotUsageErr::Http {
            kind: HttpErrKind::Status(500),
            host: "api.github.com".to_owned(),
        };
        assert!(!error.to_string().contains("test-secret"));
    }

    #[test]
    fn auth_and_failure_classification_match_the_shared_probe_tri_state() {
        use crate::agents::credits::AccountUsageReportable;

        for error in [
            CopilotUsageErr::NoCredentials,
            CopilotUsageErr::ConfigUnavailable,
            CopilotUsageErr::InvalidHost,
            CopilotUsageErr::Http {
                kind: HttpErrKind::Status(401),
                host: "api.github.com".to_owned(),
            },
        ] {
            assert!(!error.should_report(), "{error}");
        }
        for error in [
            CopilotUsageErr::Http {
                kind: HttpErrKind::Transport,
                host: "api.github.com".to_owned(),
            },
            CopilotUsageErr::UnusableSchema,
        ] {
            assert!(error.should_report(), "{error}");
        }
    }

    #[test]
    fn environment_tokens_work_without_claiming_a_logged_in_account() {
        let selected = select_credential(None, &env_with_token("env-secret"), None).unwrap();
        assert_eq!(selected.host, "github.com");
        assert_eq!(selected.identity.credentials_stamp, None);
    }
}
