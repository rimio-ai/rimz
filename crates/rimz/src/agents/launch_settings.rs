//! The one `--settings` document a launch carries. A provider that takes a JSON settings
//! object on its command line merges every RimZ contribution into the last `--settings` in
//! argv, inline or a file, and emits that one document again.

use std::path::{Path, PathBuf};

/// Merge into the last `--settings` in `args` (or an empty object), replace every
/// occurrence with the result, and return the private artifact to publish when the source
/// was a file, `private_inline` is set, or `pending` already holds one.
pub(in crate::agents) fn merge_settings(
    cwd: &Path,
    artifact_dir: &Path,
    args: &mut Vec<String>,
    pending: Option<&crate::agents::skills::LaunchSettingsArtifact>,
    private_inline: bool,
    merge: impl FnOnce(
        &mut serde_json::Map<String, serde_json::Value>,
    ) -> std::result::Result<(), String>,
) -> std::result::Result<
    Option<crate::agents::skills::LaunchSettingsArtifact>,
    crate::agents::skills::LaunchSettingsErr,
> {
    use crate::agents::skills::LaunchSettingsErr;
    let matcher = crate::agents::PresetArgMatcher::Flag(vec!["--settings".into()]);
    let value = matcher
        .occurrences(args)
        .into_iter()
        .last()
        .map(|item| item.value);
    let path = value
        .as_deref()
        .filter(|value| !value.trim_start().starts_with('{'))
        .map(|value| cwd.join(value))
        .unwrap_or_else(|| PathBuf::from("--settings"));
    let source_path = pending
        .filter(|(pending_path, _, _)| *pending_path == path)
        .map(|(_, _, source)| source)
        .unwrap_or(&path)
        .clone();
    let invalid = |reason: String| LaunchSettingsErr::Settings {
        path: source_path.clone(),
        reason,
    };
    let private = value
        .as_ref()
        .is_some_and(|value| private_inline || !value.trim_start().starts_with('{'));
    let mut settings: serde_json::Value = match (value, pending) {
        (Some(value), Some((path, settings, _))) if path == Path::new(&value) => settings.clone(),
        (Some(value), _) => {
            let bytes = if value.trim_start().starts_with('{') {
                value.into_bytes()
            } else {
                std::fs::read(&path).map_err(|error| invalid(error.to_string()))?
            };
            crate::agents::jsonc::from_slice(&bytes)
                .map_err(|error| invalid(format!("invalid JSON: {error}")))?
        }
        (None, _) => serde_json::json!({}),
    };
    let object = settings
        .as_object_mut()
        .ok_or_else(|| invalid("expected a JSON object".into()))?;
    merge(object).map_err(invalid)?;
    matcher.remove_occurrences(args);
    if private {
        use sha2::{Digest, Sha256};
        let digest = hex::encode(Sha256::digest(settings.to_string().as_bytes()));
        let path = artifact_dir.join(format!("settings.{digest}.json"));
        args.extend(["--settings".into(), path.display().to_string()]);
        return Ok(Some((path, settings, source_path)));
    }
    args.extend(["--settings".into(), settings.to_string()]);
    Ok(None)
}

/// Union `additions` into the string array `object[section][key]`, keeping first
/// occurrences.
pub(in crate::agents) fn union_settings_array(
    object: &mut serde_json::Map<String, serde_json::Value>,
    section: &str,
    key: &str,
    additions: impl IntoIterator<Item = String>,
) -> std::result::Result<(), String> {
    let section_value = object
        .entry(section)
        .or_insert_with(|| serde_json::json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("{section} must be an object"))?;
    let values = section_value
        .entry(key)
        .or_insert_with(|| serde_json::json!([]))
        .as_array_mut()
        .ok_or_else(|| format!("{section}.{key} must be an array"))?;
    values.extend(additions.into_iter().map(serde_json::Value::String));
    let mut unique = Vec::new();
    values.retain(|value| {
        if unique.contains(value) {
            return false;
        }
        unique.push(value.clone());
        true
    });
    Ok(())
}
