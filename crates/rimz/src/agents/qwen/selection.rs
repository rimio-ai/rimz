//! Effective Qwen model-provider and credential selection.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::agents::context::{AgentAccount, ProviderAccountScope};
use crate::agents::credits::file_mtime_ms;

const ALIBABA_KEY: &str = "BAILIAN_CODING_PLAN_API_KEY";
const ALIBABA_INTL_ENDPOINT: &str = "https://coding-intl.dashscope.aliyuncs.com/v1";
const ALIBABA_CN_ENDPOINT: &str = "https://coding.dashscope.aliyuncs.com/v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AlibabaRegion {
    International,
    China,
}

impl AlibabaRegion {
    pub(crate) fn scope(self) -> ProviderAccountScope {
        ProviderAccountScope::sub_provider("alibaba", self.variant())
    }

    pub(crate) fn variant(self) -> &'static str {
        match self {
            Self::International => "international",
            Self::China => "china",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::International => "Alibaba Coding Plan (International)",
            Self::China => "Alibaba Coding Plan (China)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SelectedProvider {
    Alibaba(AlibabaRegion),
    OpenAi,
    Anthropic,
    Gemini,
}

impl SelectedProvider {
    fn label(self) -> &'static str {
        match self {
            Self::Alibaba(region) => region.label(),
            Self::OpenAi => "OpenAI",
            Self::Anthropic => "Anthropic",
            Self::Gemini => "Google Gemini",
        }
    }

    fn scope(self) -> ProviderAccountScope {
        match self {
            Self::Alibaba(region) => region.scope(),
            Self::OpenAi | Self::Anthropic | Self::Gemini => ProviderAccountScope::KindWide,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CredentialSource {
    ProcessEnvironment,
    Dotenv {
        path: PathBuf,
        mtime_ms: Option<u64>,
    },
    Settings {
        path: PathBuf,
        mtime_ms: Option<u64>,
    },
}

impl CredentialSource {
    fn stamp(&self) -> Option<u64> {
        match self {
            Self::ProcessEnvironment => None,
            Self::Dotenv { mtime_ms, .. } | Self::Settings { mtime_ms, .. } => *mtime_ms,
        }
    }

    fn account_fact(&self) -> String {
        match self {
            Self::ProcessEnvironment => "process-env".to_owned(),
            Self::Dotenv { path, mtime_ms } => {
                format!("dotenv:{}:{}", path.display(), mtime_ms.unwrap_or(0))
            }
            Self::Settings { path, mtime_ms } => {
                format!("settings:{}:{}", path.display(), mtime_ms.unwrap_or(0))
            }
        }
    }
}

#[derive(Clone)]
pub(crate) struct Selection {
    pub(crate) provider: SelectedProvider,
    pub(crate) credential: String,
    credential_key: String,
    credential_source: CredentialSource,
}

impl std::fmt::Debug for Selection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Selection")
            .field("provider", &self.provider)
            .field("credential_key", &self.credential_key)
            .field("credential_source", &self.credential_source)
            .finish_non_exhaustive()
    }
}

impl Selection {
    pub(crate) fn scope(&self) -> ProviderAccountScope {
        self.provider.scope()
    }

    pub(crate) fn credentials_stamp(&self) -> Option<u64> {
        self.credential_source.stamp()
    }

    pub(crate) fn account_key(&self) -> String {
        format!(
            "provider={};variant={};key={};source={}",
            match self.provider {
                SelectedProvider::Alibaba(_) => "alibaba",
                SelectedProvider::OpenAi => "openai",
                SelectedProvider::Anthropic => "anthropic",
                SelectedProvider::Gemini => "gemini",
            },
            match self.provider {
                SelectedProvider::Alibaba(region) => region.variant(),
                _ => "direct",
            },
            self.credential_key,
            self.credential_source.account_fact(),
        )
    }

    pub(crate) fn account(&self) -> AgentAccount {
        AgentAccount {
            scope: self.scope(),
            plan: (!matches!(self.provider, SelectedProvider::Alibaba(_)))
                .then(|| "API key".to_owned()),
            account_id: None,
            metered: Some(matches!(self.provider, SelectedProvider::Alibaba(_))),
            version: None,
            sub_provider: Some(self.provider.label().to_owned()),
            credentials_updated_at_ms: self.credentials_stamp(),
        }
    }
}

#[derive(Debug)]
pub(crate) enum SelectionState {
    Found(Selection),
    LoggedOut,
    Unavailable,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Settings {
    security: Security,
    model: ModelSelection,
    model_providers: BTreeMap<String, Vec<ModelProvider>>,
    provider_protocol: BTreeMap<String, String>,
    env: BTreeMap<String, String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Security {
    auth: Auth,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct Auth {
    selected_type: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ModelSelection {
    name: Option<String>,
    #[serde(rename = "baseUrl")]
    base_url: Option<String>,
    provider: Option<String>,
}

#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ModelProvider {
    id: String,
    base_url: Option<String>,
    env_key: Option<String>,
}

pub(crate) fn resolve() -> SelectionState {
    let Ok(settings_path) = super::install::qwen_settings_path() else {
        return SelectionState::Unavailable;
    };
    let dotenv_path = settings_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".env");
    resolve_at(&settings_path, &dotenv_path, |name| {
        std::env::var(name).ok()
    })
}

fn resolve_at(
    settings_path: &Path,
    dotenv_path: &Path,
    process_env: impl Fn(&str) -> Option<String>,
) -> SelectionState {
    let bytes = match std::fs::read(settings_path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SelectionState::LoggedOut;
        }
        Err(_) => return SelectionState::Unavailable,
    };
    let settings: Settings = match crate::agents::jsonc::from_slice(&bytes) {
        Ok(settings) => settings,
        Err(_) => return SelectionState::Unavailable,
    };
    let Some(selected_type) = non_empty(settings.security.auth.selected_type.as_deref()) else {
        return SelectionState::LoggedOut;
    };
    let model_name = non_empty(settings.model.name.as_deref());

    let provider_selection = selected_model_provider(&settings, selected_type, model_name);
    let (protocol, base_url, credential_key) = match provider_selection {
        Ok(Some((protocol, model))) => {
            let Some(credential_key) =
                non_empty(model.env_key.as_deref()).or_else(|| default_credential_key(&protocol))
            else {
                return SelectionState::Unavailable;
            };
            (protocol, model.base_url.clone(), credential_key.to_owned())
        }
        Ok(None) if settings.model_providers.is_empty() => {
            let Some(key) = default_credential_key(selected_type) else {
                return SelectionState::Unavailable;
            };
            let base_url = match default_base_key(selected_type) {
                Some(base_key) => {
                    match resolve_config_value(base_key, &settings.env, dotenv_path, &process_env) {
                        Ok(value) => value,
                        Err(()) => return SelectionState::Unavailable,
                    }
                }
                None => None,
            };
            (selected_type.to_owned(), base_url, key.to_owned())
        }
        Ok(None) | Err(()) => return SelectionState::Unavailable,
    };

    let provider = match classify_provider(&protocol, base_url.as_deref()) {
        Some(provider) => provider,
        None => return SelectionState::Unavailable,
    };
    let credential = match resolve_credential(
        &credential_key,
        &settings.env,
        settings_path,
        dotenv_path,
        process_env,
    ) {
        Ok(Some(credential)) => credential,
        Ok(None) => return SelectionState::LoggedOut,
        Err(()) => return SelectionState::Unavailable,
    };
    SelectionState::Found(Selection {
        provider,
        credential: credential.0,
        credential_key,
        credential_source: credential.1,
    })
}

fn selected_model_provider<'a>(
    settings: &'a Settings,
    selected_type: &str,
    model_name: Option<&str>,
) -> Result<Option<(String, &'a ModelProvider)>, ()> {
    let Some(model_name) = model_name else {
        return Ok(None);
    };
    let explicit_provider = non_empty(settings.model.provider.as_deref());
    let selected_protocol = provider_protocol(settings, selected_type);
    let selected_base_url = non_empty(settings.model.base_url.as_deref());
    let matches = settings
        .model_providers
        .iter()
        .filter(|(provider_id, _)| {
            explicit_provider.is_some_and(|selected| selected == provider_id.as_str())
                || (explicit_provider.is_none()
                    && provider_protocol(settings, provider_id) == selected_protocol)
        })
        .flat_map(|(provider_id, models)| {
            let protocol = provider_protocol(settings, provider_id).to_owned();
            models
                .iter()
                .filter(|model| {
                    model.id == model_name
                        && selected_base_url.is_none_or(|selected| {
                            non_empty(model.base_url.as_deref()) == Some(selected)
                        })
                })
                .map(move |model| (protocol.clone(), model))
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Ok(None),
        [(protocol, model)] => Ok(Some((protocol.clone(), *model))),
        _ => Err(()),
    }
}

fn provider_protocol<'a>(settings: &'a Settings, provider_id: &'a str) -> &'a str {
    settings
        .provider_protocol
        .get(provider_id)
        .and_then(|protocol| non_empty(Some(protocol)))
        .unwrap_or(provider_id)
}

fn resolve_config_value(
    key: &str,
    settings_env: &BTreeMap<String, String>,
    dotenv_path: &Path,
    process_env: &impl Fn(&str) -> Option<String>,
) -> Result<Option<String>, ()> {
    if let Some(value) = process_env(key).and_then(non_empty_owned) {
        return Ok(Some(value));
    }
    match dotenvy::from_path_iter(dotenv_path) {
        Ok(iter) => {
            for item in iter {
                let (name, value) = item.map_err(|_| ())?;
                if name == key {
                    return Ok(non_empty_owned(value));
                }
            }
        }
        Err(error) if error.not_found() => {}
        Err(_) => return Err(()),
    }
    let value = settings_env.get(key).cloned().and_then(non_empty_owned);
    if value.as_deref().is_some_and(|value| value.starts_with('$')) {
        return Err(());
    }
    Ok(value)
}

fn resolve_credential(
    key: &str,
    settings_env: &BTreeMap<String, String>,
    settings_path: &Path,
    dotenv_path: &Path,
    process_env: impl Fn(&str) -> Option<String>,
) -> Result<Option<(String, CredentialSource)>, ()> {
    if let Some(value) = process_env(key).and_then(non_empty_owned) {
        return Ok(Some((value, CredentialSource::ProcessEnvironment)));
    }
    match dotenvy::from_path_iter(dotenv_path) {
        Ok(iter) => {
            for item in iter {
                let (name, value) = item.map_err(|_| ())?;
                if name == key
                    && let Some(value) = non_empty_owned(value)
                {
                    return Ok(Some((
                        value,
                        CredentialSource::Dotenv {
                            path: dotenv_path.to_path_buf(),
                            mtime_ms: file_mtime_ms(dotenv_path),
                        },
                    )));
                }
            }
        }
        Err(error) if error.not_found() => {}
        Err(_) => return Err(()),
    }
    let Some(value) = settings_env.get(key).cloned().and_then(non_empty_owned) else {
        return Ok(None);
    };
    if value.starts_with('$') {
        return Err(());
    }
    Ok(Some((
        value,
        CredentialSource::Settings {
            path: settings_path.to_path_buf(),
            mtime_ms: file_mtime_ms(settings_path),
        },
    )))
}

fn classify_provider(protocol: &str, base_url: Option<&str>) -> Option<SelectedProvider> {
    if let Some(region) = base_url.and_then(alibaba_region) {
        return Some(SelectedProvider::Alibaba(region));
    }
    match protocol.to_ascii_lowercase().as_str() {
        "openai" if base_url.is_none_or(|url| exact_endpoint(url, "https://api.openai.com/v1")) => {
            Some(SelectedProvider::OpenAi)
        }
        "anthropic"
            if base_url.is_none_or(|url| {
                exact_endpoint(url, "https://api.anthropic.com")
                    || exact_endpoint(url, "https://api.anthropic.com/v1")
            }) =>
        {
            Some(SelectedProvider::Anthropic)
        }
        "gemini"
            if base_url.is_none_or(|url| {
                exact_endpoint(url, "https://generativelanguage.googleapis.com")
            }) =>
        {
            Some(SelectedProvider::Gemini)
        }
        _ => None,
    }
}

fn alibaba_region(value: &str) -> Option<AlibabaRegion> {
    if exact_endpoint(value, ALIBABA_INTL_ENDPOINT) {
        Some(AlibabaRegion::International)
    } else if exact_endpoint(value, ALIBABA_CN_ENDPOINT) {
        Some(AlibabaRegion::China)
    } else {
        None
    }
}

fn exact_endpoint(value: &str, expected: &str) -> bool {
    let Ok(value) = url::Url::parse(value) else {
        return false;
    };
    let Ok(expected) = url::Url::parse(expected) else {
        return false;
    };
    value.scheme() == expected.scheme()
        && value.host_str() == expected.host_str()
        && value.port_or_known_default() == expected.port_or_known_default()
        && value.path().trim_end_matches('/') == expected.path().trim_end_matches('/')
        && value.query().is_none()
        && value.fragment().is_none()
        && value.username().is_empty()
        && value.password().is_none()
}

fn default_credential_key(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "gemini" => Some("GEMINI_API_KEY"),
        "vertex-ai" => None,
        "qwen" | "bailian" | "qwen-coding-plan" => Some(ALIBABA_KEY),
        _ => None,
    }
}

fn default_base_key(provider: &str) -> Option<&'static str> {
    match provider {
        "openai" => Some("OPENAI_BASE_URL"),
        "anthropic" => Some("ANTHROPIC_BASE_URL"),
        "gemini" => None,
        _ => None,
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn non_empty_owned(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve_fixture(settings: &str, dotenv: Option<&str>) -> SelectionState {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let dotenv_path = dir.path().join(".env");
        std::fs::write(&settings_path, settings).unwrap();
        if let Some(dotenv) = dotenv {
            std::fs::write(&dotenv_path, dotenv).unwrap();
        }
        resolve_at(&settings_path, &dotenv_path, |_| None)
    }

    fn alibaba_settings(endpoint: &str, secret: &str) -> String {
        format!(
            r#"{{
                // Qwen settings are JSONC.
                "security": {{"auth": {{"selectedType": "openai",}},}},
                "model": {{"name": "qwen3-coder-plus"}},
                "modelProviders": {{"openai": [{{
                    "id": "qwen3-coder-plus",
                    "baseUrl": "{endpoint}",
                    "envKey": "{ALIBABA_KEY}"
                }}]}},
                "env": {{"{ALIBABA_KEY}": "{secret}"}}
            }}"#
        )
    }

    #[test]
    fn resolves_both_alibaba_regions_without_exposing_secret() {
        for (endpoint, region) in [
            (ALIBABA_INTL_ENDPOINT, AlibabaRegion::International),
            (ALIBABA_CN_ENDPOINT, AlibabaRegion::China),
        ] {
            let state = resolve_fixture(&alibaba_settings(endpoint, "sentinel-secret"), None);
            let SelectionState::Found(selection) = state else {
                panic!("expected selection");
            };
            assert_eq!(selection.provider, SelectedProvider::Alibaba(region));
            assert_eq!(selection.scope(), region.scope());
            assert!(!format!("{selection:?}").contains("sentinel-secret"));
            assert!(!selection.account_key().contains("sentinel-secret"));
        }
        let custom_key = alibaba_settings(ALIBABA_INTL_ENDPOINT, "sentinel-secret")
            .replace(ALIBABA_KEY, "CUSTOM_CODING_PLAN_KEY");
        let SelectionState::Found(selection) = resolve_fixture(&custom_key, None) else {
            panic!("a manual Coding Plan provider may declare its own env key");
        };
        assert_eq!(
            selection.provider,
            SelectedProvider::Alibaba(AlibabaRegion::International)
        );
    }

    #[test]
    fn dotenv_precedes_settings_and_supplies_its_own_stamp() {
        let state = resolve_fixture(
            &alibaba_settings(ALIBABA_INTL_ENDPOINT, "settings-secret"),
            Some("BAILIAN_CODING_PLAN_API_KEY=dotenv-secret\n"),
        );
        let SelectionState::Found(selection) = state else {
            panic!("expected selection");
        };
        assert_eq!(selection.credential, "dotenv-secret");
        assert!(selection.credentials_stamp().is_some());
        assert!(selection.account_key().contains("dotenv:"));
    }

    #[test]
    fn exact_model_and_endpoint_are_required() {
        let ambiguous = alibaba_settings(ALIBABA_INTL_ENDPOINT, "secret")
            .replace(r#""id": "qwen3-coder-plus","#, r#""id": "other","#);
        assert!(matches!(
            resolve_fixture(&ambiguous, None),
            SelectionState::Unavailable
        ));
        let custom = alibaba_settings("https://proxy.invalid/v1", "secret");
        assert!(matches!(
            resolve_fixture(&custom, None),
            SelectionState::Unavailable
        ));
    }

    #[test]
    fn known_direct_provider_is_unmetered() {
        let state = resolve_fixture(
            r#"{
                "security":{"auth":{"selectedType":"anthropic"}},
                "model":{"name":"claude","baseUrl":"https://api.anthropic.com/v1"},
                "modelProviders":{"friendly-claude":[{
                    "id":"claude","envKey":"ANTHROPIC_API_KEY",
                    "baseUrl":"https://api.anthropic.com/v1"
                }]},
                "providerProtocol":{"friendly-claude":"anthropic"},
                "env":{"ANTHROPIC_API_KEY":"sentinel-secret"}
            }"#,
            None,
        );
        let SelectionState::Found(selection) = state else {
            panic!("expected selection");
        };
        assert_eq!(selection.account().metered, Some(false));
        assert!(selection.scope().is_kind_wide());
    }

    #[test]
    fn process_environment_precedes_dotenv_and_settings() {
        let dir = tempfile::tempdir().unwrap();
        let settings_path = dir.path().join("settings.json");
        let dotenv_path = dir.path().join(".env");
        std::fs::write(
            &settings_path,
            alibaba_settings(ALIBABA_INTL_ENDPOINT, "settings-secret"),
        )
        .unwrap();
        std::fs::write(&dotenv_path, format!("{ALIBABA_KEY}=dotenv-secret\n")).unwrap();
        let state = resolve_at(&settings_path, &dotenv_path, |name| {
            (name == ALIBABA_KEY).then(|| "process-secret".to_owned())
        });
        let SelectionState::Found(selection) = state else {
            panic!("expected selection");
        };
        assert_eq!(selection.credential, "process-secret");
        assert!(selection.credentials_stamp().is_none());
        assert!(selection.account_key().contains("process-env"));
        assert!(!format!("{selection:?}").contains("process-secret"));
    }

    #[test]
    fn malformed_or_ambiguous_settings_are_unavailable() {
        assert!(matches!(
            resolve_fixture("{not-json", None),
            SelectionState::Unavailable
        ));
        let ambiguous = format!(
            r#"{{
                "security":{{"auth":{{"selectedType":"openai"}}}},
                "model":{{"name":"qwen3-coder-plus"}},
                "modelProviders":{{"openai":[
                    {{"id":"qwen3-coder-plus","baseUrl":"{ALIBABA_INTL_ENDPOINT}","envKey":"{ALIBABA_KEY}"}},
                    {{"id":"qwen3-coder-plus","baseUrl":"{ALIBABA_CN_ENDPOINT}","envKey":"{ALIBABA_KEY}"}}
                ]}},
                "env":{{"{ALIBABA_KEY}":"secret"}}
            }}"#
        );
        assert!(matches!(
            resolve_fixture(&ambiguous, None),
            SelectionState::Unavailable
        ));
        let selected = ambiguous.replace(
            r#""model":{"name":"qwen3-coder-plus"}"#,
            &format!(r#""model":{{"name":"qwen3-coder-plus","baseUrl":"{ALIBABA_CN_ENDPOINT}"}}"#),
        );
        let SelectionState::Found(selection) = resolve_fixture(&selected, None) else {
            panic!("base URL must disambiguate the selected provider model");
        };
        assert_eq!(
            selection.provider,
            SelectedProvider::Alibaba(AlibabaRegion::China)
        );
    }

    #[test]
    fn missing_selection_or_credentials_are_logged_out() {
        assert!(matches!(
            resolve_fixture("{}", None),
            SelectionState::LoggedOut
        ));
        let settings = alibaba_settings(ALIBABA_CN_ENDPOINT, "secret").replace(
            r#""env": {"BAILIAN_CODING_PLAN_API_KEY": "secret"}"#,
            r#""env": {}"#,
        );
        assert!(matches!(
            resolve_fixture(&settings, None),
            SelectionState::LoggedOut
        ));
    }
}
