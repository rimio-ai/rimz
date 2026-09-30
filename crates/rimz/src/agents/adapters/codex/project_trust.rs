//! Codex `[projects]` directory-trust decisions and lossless grant previews.
//!
//! Check exact cwd, nearest project root, then the main Git root supplied by the launch resolver; never grant trust.
//! Hook preflight normally catches file-level failures first; this read still reports them if the config changes between checks.

use std::collections::HashMap;
use std::path::Path;

use crate::agents::{FolderTrust, FolderTrustGap, FolderTrustPreview};
use serde::Deserialize;

#[derive(Deserialize)]
struct ProjectTrustConfig {
    #[serde(default)]
    projects: HashMap<String, ProjectConfig>,
    project_root_markers: Option<Vec<String>>,
}

#[derive(Deserialize)]
struct ProjectConfig {
    trust_level: Option<TrustLevel>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum TrustLevel {
    Trusted,
    Untrusted,
}

pub(super) fn trust_gap_at(config: &Path, cwd: &Path, repo_root: Option<&Path>) -> FolderTrust {
    let parsed = (|| {
        let original = crate::agents::locate::read_optional_file("codex", config)
            .map_err(|err| err.to_string())?;
        let text = original.as_deref().unwrap_or("");
        let trust = toml::from_str::<ProjectTrustConfig>(text).map_err(|err| err.to_string())?;
        let document = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|err| err.to_string())?;
        Ok::<_, String>((original, trust, document))
    })();
    let (original, trust, mut document) = match parsed {
        Ok(parsed) => parsed,
        Err(err) => {
            return FolderTrust::Undecided(FolderTrustGap {
                path: config.into(),
                key: repo_root
                    .unwrap_or(cwd)
                    .canonicalize()
                    .unwrap_or_else(|_| repo_root.unwrap_or(cwd).into()),
                grant: Err(format!(
                    "repair or make readable `{}`: {err}",
                    config.display()
                )),
            });
        }
    };
    let default_markers = [".git".to_owned()];
    let markers = trust
        .project_root_markers
        .as_deref()
        .unwrap_or(&default_markers);
    let project_root = cwd
        .ancestors()
        .find(|ancestor| {
            markers.iter().any(|marker| {
                let path = ancestor.join(marker);
                let Ok(metadata) = path.metadata() else {
                    return false;
                };
                marker != ".git" || !metadata.is_dir() || path.join("HEAD").metadata().is_ok()
            })
        })
        .unwrap_or(cwd);

    for candidate in [Some(cwd), Some(project_root), repo_root]
        .into_iter()
        .flatten()
    {
        let canonical = candidate.canonicalize().ok();
        for key in canonical
            .as_deref()
            .into_iter()
            .chain(std::iter::once(candidate))
        {
            if trust
                .projects
                .get(key.to_string_lossy().as_ref())
                .is_some_and(|project| project.trust_level.is_some())
            {
                return FolderTrust::Decided;
            }
        }
    }

    let key = repo_root.unwrap_or(project_root);
    let key = key.canonicalize().unwrap_or_else(|_| key.into());
    if !document.contains_key("projects") {
        let mut projects = toml_edit::Table::new();
        projects.set_implicit(true);
        document["projects"] = toml_edit::Item::Table(projects);
    }
    if document["projects"]
        .get(key.to_string_lossy().as_ref())
        .is_none()
    {
        let mut project = toml_edit::Table::new();
        project.set_position(Some(isize::MAX));
        if document["projects"].is_table() {
            let mut prefix = document.trailing().as_str().unwrap_or("").to_owned();
            if !prefix.is_empty() && !prefix.ends_with('\n') {
                prefix.push('\n');
            }
            project.decor_mut().set_prefix(prefix);
            document.set_trailing("");
        }
        document["projects"][key.to_string_lossy().as_ref()] = toml_edit::Item::Table(project);
    }
    document["projects"][key.to_string_lossy().as_ref()]["trust_level"] =
        toml_edit::value("trusted");
    let candidate = document.to_string();
    let grant = toml::from_str::<ProjectTrustConfig>(&candidate)
        .map_err(|err| format!("repair or make readable `{}`: {err}", config.display()))
        .and_then(|trust| {
            if !trust
                .projects
                .get(key.to_string_lossy().as_ref())
                .is_some_and(|project| matches!(project.trust_level, Some(TrustLevel::Trusted)))
            {
                return Err(format!(
                    "could not verify folder-trust preview for `{}`",
                    config.display()
                ));
            }
            Ok(FolderTrustPreview {
                original,
                candidate,
            })
        });
    FolderTrust::Undecided(FolderTrustGap {
        path: config.into(),
        key,
        grant,
    })
}
