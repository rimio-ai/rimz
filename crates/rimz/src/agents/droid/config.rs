//! Read-only projection of Factory custom-model settings.
//!
//! Only display/pricing identity and context capacity cross this boundary.
//! Credentials, endpoints, provider options, and environment interpolation are
//! intentionally absent from the typed projection and from every error path.

use std::path::Path;

use serde::Deserialize;

use super::transcript;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ResolvedCustomModel {
    pub display_name: String,
    pub model_id: String,
    pub max_context_limit: Option<u64>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct SettingsProjection {
    #[serde(rename = "customModels")]
    custom_models: Option<Vec<CustomModel>>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct LegacyProjection {
    custom_models: Option<Vec<LegacyCustomModel>>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct CustomModel {
    id: Option<String>,
    #[serde(rename = "displayName")]
    display_name: Option<String>,
    model: Option<String>,
    #[serde(rename = "maxContextLimit")]
    max_context_limit: Option<u64>,
}

#[derive(Clone, Default, Deserialize)]
#[serde(default)]
struct LegacyCustomModel {
    display_name: Option<String>,
    model: Option<String>,
    max_context_limit: Option<u64>,
}

impl From<LegacyCustomModel> for CustomModel {
    fn from(model: LegacyCustomModel) -> Self {
        Self {
            id: None,
            display_name: model.display_name,
            model: model.model,
            max_context_limit: model.max_context_limit,
        }
    }
}

/// Resolve one raw Factory custom selector through the current settings
/// hierarchy. Any unreadable or malformed present source makes the result
/// unknown; enrichment abstains rather than borrowing stale identity from a
/// lower-precedence file.
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
    let layers = current_layers(user_settings, &cwd)?;

    // Stable ids are authoritative. A duplicate at one precedence tier is a
    // conflict; a lower tier cannot override a proven higher-tier match.
    for layer in &layers {
        let matches = layer
            .iter()
            .filter(|model| non_empty_opt(model.id.as_deref()) == Some(selector))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [] => {}
            [model] => return resolved(model),
            _ => return None,
        }
    }

    // Index-bearing selectors predate stable ids. Mixing the two generations
    // makes the reconstructed index ambiguous, so legacy reconstruction is
    // allowed only for an all-legacy current catalogue.
    let current_entries = layers.iter().flatten().collect::<Vec<_>>();
    if current_entries
        .iter()
        .any(|model| non_empty_opt(model.id.as_deref()).is_some())
    {
        return None;
    }
    if !current_entries.is_empty() {
        return unique_legacy_match(selector, &layers);
    }

    // `settings*.json` owns the current catalogue. Only an entirely absent
    // current catalogue falls back to the legacy user `config.json` shape.
    let legacy_path = user_settings.with_file_name("config.json");
    let legacy = read_optional::<LegacyProjection>(&legacy_path)?
        .and_then(|projection| projection.custom_models)
        .unwrap_or_default()
        .into_iter()
        .map(CustomModel::from)
        .collect::<Vec<_>>();
    unique_legacy_match(selector, &[legacy])
}

fn current_layers(user_settings: &Path, cwd: &Path) -> Option<Vec<Vec<CustomModel>>> {
    let user_local = user_settings.with_file_name("settings.local.json");
    let project_settings = cwd.join(".factory/settings.json");
    let project_local = cwd.join(".factory/settings.local.json");
    // Highest precedence first: project-local, project, user-local, user.
    [
        project_local,
        project_settings,
        user_local,
        user_settings.to_path_buf(),
    ]
    .into_iter()
    .map(|path| {
        read_optional::<SettingsProjection>(&path).map(|projection| {
            projection
                .and_then(|projection| projection.custom_models)
                .unwrap_or_default()
        })
    })
    .collect()
}

fn read_optional<T: serde::de::DeserializeOwned>(path: &Path) -> Option<Option<T>> {
    if !path.exists() {
        return Some(None);
    }
    let bytes = std::fs::read(path).ok()?;
    crate::agents::jsonc::from_slice(&bytes).ok().map(Some)
}

fn unique_legacy_match(selector: &str, layers: &[Vec<CustomModel>]) -> Option<ResolvedCustomModel> {
    let matches = layers
        .iter()
        .flat_map(|models| models.iter().enumerate())
        .filter_map(|(index, model)| {
            let display = non_empty_opt(model.display_name.as_deref())?;
            let reconstructed = format!("custom:{}-{index}", display.replace(' ', "-"));
            (reconstructed == selector)
                .then(|| resolved(model))
                .flatten()
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [model] => Some(model.clone()),
        _ => None,
    }
}

fn resolved(model: &CustomModel) -> Option<ResolvedCustomModel> {
    Some(ResolvedCustomModel {
        display_name: non_empty_opt(model.display_name.as_deref())?.to_owned(),
        model_id: non_empty_opt(model.model.as_deref())?.to_owned(),
        max_context_limit: model.max_context_limit.filter(|limit| *limit > 0),
    })
}

fn non_empty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn non_empty_opt(value: Option<&str>) -> Option<&str> {
    value.and_then(non_empty)
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
            r#"{"customModels":[{"id":"custom:deepseek","displayName":"User DeepSeek","model":"old-model","apiKey":"secret-user"}]}"#,
        );
        write(
            &cwd.join(".factory/settings.local.json"),
            r#"{// comment
              "customModels":[{"id":"custom:deepseek","displayName":"DeepSeek V4 Pro","model":"deepseek-v4-pro","maxContextLimit":200000,"baseUrl":"https://secret.invalid","apiKey":"${SECRET}"}],
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
    fn legacy_selector_reconstruction_preserves_spaces_hyphens_and_index() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let user = root.path().join("user/settings.json");
        let transcript = session(root.path(), &cwd);
        write(
            &user,
            r#"{"customModels":[
              {"displayName":"Other","model":"other"},
              {"displayName":"DeepSeek V4-Pro","model":"deepseek-v4-pro","maxContextLimit":128000}
            ]}"#,
        );

        let resolved =
            resolve_custom_model("custom:DeepSeek-V4-Pro-1", &transcript, &user).unwrap();
        assert_eq!(resolved.display_name, "DeepSeek V4-Pro");
        assert!(resolve_custom_model("custom:DeepSeek-V4-Pro-0", &transcript, &user).is_none());

        write(
            &user,
            r#"{"customModels":[
              {"displayName":"DeepSeek V4-Pro","model":"deepseek-v4-pro","maxContextLimit":128000},
              {"displayName":"Other","model":"other"}
            ]}"#,
        );
        assert!(
            resolve_custom_model("custom:DeepSeek-V4-Pro-1", &transcript, &user).is_none(),
            "a selector with a stale index must not borrow the reordered entry"
        );
    }

    #[test]
    fn ambiguous_or_malformed_sources_abstain_without_falling_back() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let user = root.path().join("user/settings.json");
        let transcript = session(root.path(), &cwd);
        write(
            &user,
            r#"{"customModels":[{"displayName":"Same","model":"one"}]}"#,
        );
        write(
            &cwd.join(".factory/settings.json"),
            r#"{"customModels":[{"displayName":"Same","model":"two"}]}"#,
        );
        assert!(resolve_custom_model("custom:Same-0", &transcript, &user).is_none());

        write(&cwd.join(".factory/settings.local.json"), "{ malformed");
        assert!(resolve_custom_model("custom:Same-0", &transcript, &user).is_none());
    }

    #[test]
    fn legacy_user_config_is_only_a_current_settings_fallback() {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().join("project");
        let user = root.path().join("user/settings.json");
        let transcript = session(root.path(), &cwd);
        write(
            &user.with_file_name("config.json"),
            r#"{"custom_models":[{"display_name":"Legacy Model","model":"legacy-model","max_context_limit":64000}]}"#,
        );
        let resolved = resolve_custom_model("custom:Legacy-Model-0", &transcript, &user).unwrap();
        assert_eq!(resolved.model_id, "legacy-model");

        write(
            &user,
            r#"{"customModels":[{"displayName":"Current","model":"current"}]}"#,
        );
        assert!(resolve_custom_model("custom:Legacy-Model-0", &transcript, &user).is_none());
    }
}
