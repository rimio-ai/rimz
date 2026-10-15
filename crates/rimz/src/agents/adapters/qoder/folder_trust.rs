//! Read-only folder decisions and strict, explicitly approved trust previews.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value};

use crate::agents::{FolderTrust, FolderTrustGap, FolderTrustPreview};

pub(super) fn folder_trust(cwd: &Path, env: &BTreeMap<String, String>) -> FolderTrust {
    let key = cwd.canonicalize().unwrap_or_else(|_| cwd.into());
    let path = match super::install::settings_path(env) {
        Ok(path) => path,
        Err(err) => {
            return FolderTrust::Undecided(FolderTrustGap {
                path: Default::default(),
                key,
                grant: Err(err.to_string()),
            });
        }
    };
    let parsed = crate::agents::locate::read_optional_file("qoder", &path)
        .map_err(|err| err.to_string())
        .and_then(|original| {
            let value: Value =
                crate::agents::jsonc::from_slice(original.as_deref().unwrap_or("{}").as_bytes())
                    .map_err(|err| err.to_string())?;
            if !value.is_object() {
                return Err("expected a JSON object".into());
            }
            Ok((original, value))
        });
    let grant = match parsed {
        Ok((_, value)) if trusts(&value, &key) => return FolderTrust::Decided,
        Ok((original, _)) => preview(original, &key),
        Err(err) => Err(err),
    }
    .map_err(|err| format!("repair `{}`: {err}", path.display()));
    FolderTrust::Undecided(FolderTrustGap { path, key, grant })
}

/// Qoder's own match: folder trust switched off, or a trusted or additional
/// directory that is `key` or a component-wise ancestor of it.
fn trusts(settings: &Value, key: &Path) -> bool {
    if settings.pointer("/security/folderTrust/enabled") == Some(&Value::Bool(false)) {
        return true;
    }
    ["trustDirectories", "additionalDirectories"]
        .iter()
        .filter_map(|name| settings.get("permissions")?.get(*name)?.as_array())
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|entry| Path::new(entry).canonicalize().ok())
        .any(|entry| key.starts_with(entry))
}

fn preview(original: Option<String>, key: &Path) -> Result<FolderTrustPreview, String> {
    let mut root: Map<String, Value> =
        serde_json::from_str(original.as_deref().unwrap_or("{}")).map_err(|err| err.to_string())?;
    let permissions = root
        .entry("permissions")
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or("expected permissions object")?;
    let entries = permissions
        .entry("trustDirectories")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or("expected permissions.trustDirectories array")?;
    entries.push(Value::String(key.to_string_lossy().into_owned()));
    let candidate = serde_json::to_string_pretty(&root).map_err(|err| err.to_string())? + "\n";
    Ok(FolderTrustPreview {
        original,
        candidate,
    })
}
