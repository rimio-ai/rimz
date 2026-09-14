//! Read-only projection of Factory custom-model settings.
//!
//! Only display/pricing identity and context capacity cross this boundary.
//! Credentials, endpoints, provider options, and environment interpolation are
//! intentionally absent from the typed projection and from every error path:
//! the load filter reads their presence, and the placeholder-key check reads
//! the key only to compare it.
//!
//! The catalogue mirrors Droid 0.218.2: each settings file loads and names its
//! own entries, `settings.local.json` replaces its sibling's list, the folder's
//! legacy `config.json` appends entries it does not already hold, and the
//! project folder precedes the user folder with duplicate ids dropped.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Deserializer};
use serde_json::Value;

#[cfg(test)]
use super::transcript;
use crate::agents::transcript_fs::deserialize_optional_u64_lossy;

/// Template keys Droid ships in sample configs; entries carrying one are dropped.
const PLACEHOLDER_API_KEYS: [&str; 2] = ["YOUR_API_KEY", "YOUR_OPENAI_API_KEY"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ResolvedCustomModel {
    pub display_name: String,
    pub model_id: String,
    pub max_context_limit: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct SettingsProjection {
    #[serde(rename = "customModels")]
    custom_models: Option<Vec<Value>>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct LegacyProjection {
    custom_models: Option<Vec<Value>>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CustomModel {
    #[serde(deserialize_with = "string_only")]
    id: Option<String>,
    #[serde(rename = "displayName", deserialize_with = "string_only")]
    display_name: Option<String>,
    #[serde(deserialize_with = "string_only")]
    model: Option<String>,
    #[serde(rename = "provider", deserialize_with = "is_string")]
    has_provider: bool,
    #[serde(rename = "baseUrl", deserialize_with = "is_string")]
    has_base_url: bool,
    #[serde(rename = "bedrock", deserialize_with = "is_object")]
    has_bedrock: bool,
    #[serde(rename = "apiKey", deserialize_with = "is_placeholder_key")]
    placeholder_key: bool,
    #[serde(deserialize_with = "deserialize_optional_u64_lossy")]
    index: Option<u64>,
    #[serde(
        rename = "maxContextLimit",
        deserialize_with = "deserialize_optional_u64_lossy"
    )]
    max_context_limit: Option<u64>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct LegacyCustomModel {
    #[serde(deserialize_with = "string_only")]
    model_display_name: Option<String>,
    #[serde(deserialize_with = "string_only")]
    model: Option<String>,
    #[serde(rename = "provider", deserialize_with = "is_string")]
    has_provider: bool,
    #[serde(rename = "base_url", deserialize_with = "is_string")]
    has_base_url: bool,
    #[serde(rename = "bedrock", deserialize_with = "is_object")]
    has_bedrock: bool,
    #[serde(rename = "api_key", deserialize_with = "is_placeholder_key")]
    placeholder_key: bool,
    #[serde(deserialize_with = "deserialize_optional_u64_lossy")]
    max_context_limit: Option<u64>,
}

/// One entry that survived Droid's load filter, before its id is assigned.
struct LoadedModel {
    stored_id: Option<String>,
    name: String,
    model: String,
    index: Option<u64>,
    max_context_limit: Option<u64>,
}

struct CatalogEntry {
    id: String,
    index: u64,
    name: String,
    model: String,
    max_context_limit: Option<u64>,
}

impl CustomModel {
    fn load(self) -> Option<LoadedModel> {
        let model = self.model?;
        if !self.has_provider || !(self.has_base_url || self.has_bedrock) || self.placeholder_key {
            return None;
        }
        Some(LoadedModel {
            stored_id: self.id,
            name: self.display_name.unwrap_or_else(|| model.clone()),
            model,
            index: self.index,
            max_context_limit: self.max_context_limit,
        })
    }
}

impl LegacyCustomModel {
    fn load(self) -> Option<LoadedModel> {
        let model = self.model?;
        if !self.has_provider || !(self.has_base_url || self.has_bedrock) || self.placeholder_key {
            return None;
        }
        Some(LoadedModel {
            stored_id: None,
            name: self
                .model_display_name
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| model.clone()),
            model,
            index: None,
            max_context_limit: self.max_context_limit,
        })
    }
}

/// Resolve one raw Factory custom selector through the current settings
/// hierarchy. Any unreadable or malformed present settings file makes the
/// result unknown; enrichment abstains rather than borrowing identity from a
/// catalogue Droid itself would not have loaded.
#[cfg(test)]
pub(super) fn resolve_custom_model(
    selector: &str,
    session_path: &Path,
    user_settings: &Path,
) -> Option<ResolvedCustomModel> {
    let selector = non_empty(selector)?;
    if !selector.starts_with("custom:") {
        return None;
    }
    let cwd = transcript::session_cwd(session_path)?;
    resolve_custom_model_from_cwd(selector, &cwd, user_settings)
}

pub(super) fn resolve_custom_model_from_cwd(
    selector: &str,
    cwd: &Path,
    user_settings: &Path,
) -> Option<ResolvedCustomModel> {
    let selector = non_empty(selector)?;
    let raw_model = selector.strip_prefix("custom:")?;
    if !cwd.is_absolute() {
        return None;
    }
    let mut catalog = folder_catalog(&cwd.join(".factory/settings.json"))?;
    for entry in folder_catalog(user_settings)? {
        if !catalog.iter().any(|held| held.id == entry.id) {
            catalog.push(entry);
        }
    }

    if let Some(entry) = catalog.iter().find(|entry| entry.id == selector) {
        return resolved(entry);
    }
    if let Some(name) = generated_selector_name(raw_model) {
        let by_index = catalog.iter().find(|entry| {
            let prefix = format!("custom:{}-", slug(&entry.name));
            entry.id.starts_with(&prefix) && format!("{prefix}{}", entry.index) == selector
        });
        if let Some(entry) = by_index {
            return resolved(entry);
        }
        let by_name = catalog
            .iter()
            .filter(|entry| slug(&entry.name) == name)
            .collect::<Vec<_>>();
        if let [entry] = by_name.as_slice()
            && entry.id.starts_with(&format!("custom:{name}-"))
        {
            return resolved(entry);
        }
    }
    catalog
        .iter()
        .find(|entry| entry.model == raw_model)
        .and_then(resolved)
}

/// One settings folder's effective catalogue, named by its `settings.json`.
fn folder_catalog(settings: &Path) -> Option<Vec<CatalogEntry>> {
    let base = read_optional::<SettingsProjection>(settings)?;
    let local =
        read_optional::<SettingsProjection>(&settings.with_file_name("settings.local.json"))?;
    let current = local
        .and_then(|projection| projection.custom_models)
        .or_else(|| base.and_then(|projection| projection.custom_models))
        .unwrap_or_default();
    let mut catalog = assign_ids(
        current
            .into_iter()
            .filter_map(|entry| serde_json::from_value::<CustomModel>(entry).ok())
            .filter_map(CustomModel::load),
    );

    // Droid skips an unreadable legacy file rather than failing the folder.
    let legacy = read_optional::<LegacyProjection>(&settings.with_file_name("config.json"))
        .flatten()
        .and_then(|projection| projection.custom_models)
        .unwrap_or_default();
    let legacy = assign_ids(
        legacy
            .into_iter()
            .filter_map(|entry| serde_json::from_value::<LegacyCustomModel>(entry).ok())
            .filter_map(LegacyCustomModel::load),
    );
    for entry in legacy {
        if !catalog
            .iter()
            .any(|held| held.model == entry.model || held.id == entry.id)
        {
            catalog.push(entry);
        }
    }
    Some(catalog)
}

/// Droid's generated id: `custom:<slug>-<n>`, where `n` counts earlier entries
/// of the same file sharing the slug. A stored id wins.
fn assign_ids(loaded: impl Iterator<Item = LoadedModel>) -> Vec<CatalogEntry> {
    let mut ordinals = HashMap::<String, u64>::new();
    loaded
        .enumerate()
        .map(|(position, model)| {
            let slug = slug(&model.name);
            let ordinal = ordinals.entry(slug.clone()).or_default();
            let generated = format!("custom:{slug}-{ordinal}");
            *ordinal += 1;
            CatalogEntry {
                id: model.stored_id.unwrap_or(generated),
                index: model.index.unwrap_or(position as u64),
                name: model.name,
                model: model.model,
                max_context_limit: model.max_context_limit,
            }
        })
        .collect()
}

fn slug(name: &str) -> String {
    name.split_whitespace().collect::<Vec<_>>().join("-")
}

/// The name part of a `<name>-<n>` selector body, when `n` is a canonical
/// non-negative integer.
fn generated_selector_name(body: &str) -> Option<&str> {
    let (name, ordinal) = body.rsplit_once('-')?;
    let canonical = ordinal
        .parse::<u64>()
        .is_ok_and(|value| value.to_string() == ordinal);
    (!name.is_empty() && canonical).then_some(name)
}

fn read_optional<T: serde::de::DeserializeOwned>(path: &Path) -> Option<Option<T>> {
    if !path.exists() {
        return Some(None);
    }
    let bytes = std::fs::read(path).ok()?;
    crate::agents::jsonc::from_slice(&bytes).ok().map(Some)
}

fn resolved(entry: &CatalogEntry) -> Option<ResolvedCustomModel> {
    let model_id = non_empty(&entry.model)?.to_owned();
    Some(ResolvedCustomModel {
        display_name: non_empty(&entry.name).unwrap_or(&model_id).to_owned(),
        model_id,
        max_context_limit: entry.max_context_limit.filter(|limit| *limit > 0),
    })
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn string_only<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(deserializer)? {
        Some(Value::String(value)) => Some(value),
        _ => None,
    })
}

fn is_string<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(matches!(
        Option::<Value>::deserialize(deserializer)?,
        Some(Value::String(_))
    ))
}

fn is_object<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(matches!(
        Option::<Value>::deserialize(deserializer)?,
        Some(Value::Object(_))
    ))
}

fn is_placeholder_key<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(matches!(
        Option::<Value>::deserialize(deserializer)?,
        Some(Value::String(key)) if PLACEHOLDER_API_KEYS.contains(&key.as_str())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(root: &Path, cwd: &Path) -> std::path::PathBuf {
        let path = root.join("session.jsonl");
        std::fs::write(
            &path,
            format!(
                "{{\"type\":\"session_start\",\"version\":2,\"cwd\":{}}}\n",
                serde_json::to_string(&cwd.to_string_lossy()).unwrap()
            ),
        )
        .unwrap();
        path
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn exact_id_uses_highest_precedence_and_exposes_no_secret_fields() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let user = root.path().join("user/settings.json");
        let transcript = session(root.path(), &cwd);
        write(
            &user,
            r#"{"customModels":[{"id":"custom:deepseek","displayName":"User DeepSeek","model":"old-model","provider":"openai","baseUrl":"https://user.invalid","apiKey":"secret-user"}]}"#,
        );
        write(
            &cwd.join(".factory/settings.local.json"),
            r#"{// comment
              "customModels":[{"id":"custom:deepseek","displayName":"DeepSeek V4 Pro","model":"deepseek-v4-pro","provider":"openai","maxContextLimit":200000,"baseUrl":"https://secret.invalid","apiKey":"${SECRET}"}],
            }"#,
        );

        let resolved = resolve_custom_model("custom:deepseek", &transcript, &user).unwrap();
        assert_eq!(resolved.display_name, "DeepSeek V4 Pro");
        assert_eq!(resolved.model_id, "deepseek-v4-pro");
        assert_eq!(resolved.max_context_limit, Some(200_000));
        let debug = format!("{resolved:?}");
        assert!(!debug.contains("secret"));
        assert!(!debug.contains("baseUrl"));
    }

    #[test]
    fn generated_ids_count_per_name_and_skip_filtered_entries() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let user = root.path().join("user/settings.json");
        let transcript = session(root.path(), &cwd);
        write(
            &user,
            r#"{"customModels":[
              {"displayName":"Other","model":"other","provider":"openai","baseUrl":"https://o.invalid"},
              {"displayName":"Draft","model":"draft","provider":"openai","baseUrl":"https://d.invalid","apiKey":"YOUR_API_KEY"},
              {"displayName":"No Endpoint","model":"none","provider":"openai"},
              {"id":"custom:pinned","displayName":"DeepSeek  V4","model":"pinned","provider":"openai","baseUrl":"https://p.invalid"},
              {"displayName":" DeepSeek V4 ","model":"deepseek-v4-pro","provider":"anthropic","bedrock":{},"maxContextLimit":128000},
              {"model":"bare-model","provider":"openai","baseUrl":"https://b.invalid"}
            ]}"#,
        );

        let resolved = resolve_custom_model("custom:DeepSeek-V4-1", &transcript, &user).unwrap();
        assert_eq!(resolved.model_id, "deepseek-v4-pro");
        assert_eq!(resolved.display_name, "DeepSeek V4");
        assert_eq!(resolved.max_context_limit, Some(128_000));
        assert_eq!(
            resolve_custom_model("custom:bare-model-0", &transcript, &user)
                .unwrap()
                .display_name,
            "bare-model"
        );
        assert_eq!(
            resolve_custom_model("custom:pinned", &transcript, &user)
                .unwrap()
                .model_id,
            "pinned"
        );
        assert!(resolve_custom_model("custom:Draft-0", &transcript, &user).is_none());
        assert!(resolve_custom_model("custom:No-Endpoint-0", &transcript, &user).is_none());
        assert_eq!(
            resolve_custom_model("custom:other", &transcript, &user)
                .unwrap()
                .display_name,
            "Other",
            "a bare custom model selector falls back to the model field"
        );
    }

    #[test]
    fn stale_selectors_fall_back_by_index_then_unique_name() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let user = root.path().join("user/settings.json");
        let transcript = session(root.path(), &cwd);
        write(
            &user,
            r#"{"customModels":[
              {"displayName":"Other","model":"other","provider":"openai","baseUrl":"https://o.invalid"},
              {"displayName":"Solo","model":"solo","provider":"openai","baseUrl":"https://s.invalid"}
            ]}"#,
        );
        assert_eq!(
            resolve_custom_model("custom:Solo-1", &transcript, &user)
                .unwrap()
                .model_id,
            "solo",
            "a pre-ordinal selector carries the entry's position"
        );
        assert_eq!(
            resolve_custom_model("custom:Solo-7", &transcript, &user)
                .unwrap()
                .model_id,
            "solo"
        );
        assert!(resolve_custom_model("custom:Missing-0", &transcript, &user).is_none());
    }

    #[test]
    fn folders_merge_by_id_and_malformed_settings_abstain() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let user = root.path().join("user/settings.json");
        let transcript = session(root.path(), &cwd);
        write(
            &user,
            r#"{"customModels":[{"displayName":"Same","model":"user","provider":"openai","baseUrl":"https://u.invalid"},{"displayName":"User Only","model":"user-only","provider":"openai","baseUrl":"https://u.invalid"}]}"#,
        );
        write(
            &cwd.join(".factory/settings.json"),
            r#"{"customModels":[{"displayName":"Same","model":"project","provider":"openai","baseUrl":"https://p.invalid"}]}"#,
        );
        assert_eq!(
            resolve_custom_model("custom:Same-0", &transcript, &user)
                .unwrap()
                .model_id,
            "project"
        );
        assert_eq!(
            resolve_custom_model("custom:User-Only-0", &transcript, &user)
                .unwrap()
                .model_id,
            "user-only"
        );

        write(
            &cwd.join(".factory/settings.local.json"),
            r#"{"customModels":[{"displayName":"Local","model":"local","provider":"openai","baseUrl":"https://l.invalid"}]}"#,
        );
        assert_eq!(
            resolve_custom_model("custom:Same-0", &transcript, &user)
                .unwrap()
                .model_id,
            "user",
            "settings.local.json replaces its sibling's list"
        );

        write(&cwd.join(".factory/settings.local.json"), "{ malformed");
        assert!(resolve_custom_model("custom:Same-0", &transcript, &user).is_none());
    }

    #[test]
    fn legacy_config_appends_entries_the_current_catalogue_lacks() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let user = root.path().join("user/settings.json");
        let transcript = session(root.path(), &cwd);
        write(
            &user.with_file_name("config.json"),
            r#"{"custom_models":[
              {"model_display_name":"Legacy Model","model":"legacy-model","provider":"openai","base_url":"https://l.invalid","max_context_limit":64000},
              {"model_display_name":"Shadowed","model":"current","provider":"openai","base_url":"https://l.invalid"}
            ]}"#,
        );
        let resolved = resolve_custom_model("custom:Legacy-Model-0", &transcript, &user).unwrap();
        assert_eq!(resolved.model_id, "legacy-model");
        assert_eq!(resolved.max_context_limit, Some(64_000));

        write(
            &user,
            r#"{"customModels":[{"displayName":"Current","model":"current","provider":"openai","baseUrl":"https://c.invalid"}]}"#,
        );
        assert_eq!(
            resolve_custom_model("custom:Legacy-Model-0", &transcript, &user)
                .unwrap()
                .model_id,
            "legacy-model"
        );
        assert!(
            resolve_custom_model("custom:Shadowed-0", &transcript, &user).is_none(),
            "a legacy entry whose model the current catalogue holds is dropped"
        );
    }
}
