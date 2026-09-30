//! Claude folder trust under the room's selected login, without writes.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value;

use crate::agents::{FolderTrust, FolderTrustGap, FolderTrustPreview};

pub(super) fn folder_trust(
    cwd: &Path,
    repo_root: Option<&Path>,
    login_env: &BTreeMap<String, String>,
) -> FolderTrust {
    let key = repo_root.unwrap_or(cwd);
    let key = key.canonicalize().unwrap_or_else(|_| key.into());
    let Some(path) = super::remote_consent::global_config_path(login_env) else {
        return FolderTrust::Undecided(FolderTrustGap {
            path: Default::default(),
            key,
            grant: Err("set HOME or CLAUDE_CONFIG_DIR to the Claude config directory".into()),
        });
    };
    let home = login_env
        .get("HOME")
        .filter(|home| !home.is_empty())
        .map(Path::new);
    if home.is_some_and(|home| home.canonicalize().unwrap_or_else(|_| home.into()) == key) {
        return FolderTrust::Undecided(FolderTrustGap {
            path,
            key,
            grant: Err("Claude never persists trust for the home directory; start Claude in a subdirectory".into()),
        });
    }
    let original = crate::agents::locate::read_optional_file("claude", &path);
    let parsed = original
        .map_err(|err| err.to_string())
        .and_then(|original| {
            let value = serde_json::from_str::<Value>(original.as_deref().unwrap_or("{}"))
                .map_err(|err| err.to_string())?;
            if !value.is_object() {
                return Err("expected a JSON object".into());
            }
            Ok((original, value))
        });
    let (original, value) = match parsed {
        Ok(parsed) => parsed,
        Err(err) => {
            return FolderTrust::Undecided(FolderTrustGap {
                grant: Err(format!("repair `{}`: {err}", path.display())),
                path,
                key,
            });
        }
    };
    let candidates = repo_root
        .into_iter()
        .chain(cwd.ancestors().filter(|_| repo_root.is_none()));
    for candidate in candidates {
        let canonical = candidate.canonicalize().ok();
        for candidate in canonical
            .as_deref()
            .into_iter()
            .chain(std::iter::once(candidate))
        {
            if value["projects"][candidate.to_string_lossy().as_ref()]["hasTrustDialogAccepted"]
                == true
            {
                return FolderTrust::Decided;
            }
        }
    }
    let grant = super::json_edit::set_true(
        original.as_deref().unwrap_or("{}"),
        &[
            "projects",
            key.to_string_lossy().as_ref(),
            "hasTrustDialogAccepted",
        ],
    )
    .map(|candidate| FolderTrustPreview {
        original,
        candidate,
    })
    .ok_or_else(|| {
        format!(
            "repair `{}`: expected project trust objects",
            path.display()
        )
    });
    FolderTrust::Undecided(FolderTrustGap { path, key, grant })
}
