//! Read-only Codex `[projects]` directory trust, following upstream `codex-rs/config/src/loader/mod.rs` and `codex-rs/git-utils/src/trust.rs`.
//!
//! Check exact cwd, nearest project root, then the main Git root supplied by the launch resolver; never grant trust.

use std::collections::HashMap;
use std::path::Path;

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

pub(super) fn trust_gap_at(config: &Path, cwd: &Path, repo_root: Option<&Path>) -> Option<String> {
    let root = match super::install::read_existing_table(config) {
        Ok(root) => root,
        Err(err) => {
            return Some(format!(
                "repair or make readable `{}`: {err}",
                config.display()
            ));
        }
    };
    let trust: ProjectTrustConfig = match toml::Value::Table(root).try_into() {
        Ok(trust) => trust,
        Err(err) => {
            return Some(format!(
                "repair Codex project trust configuration in `{}`: {err}",
                config.display()
            ));
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
                return None;
            }
        }
    }

    let key = repo_root.unwrap_or(project_root);
    let canonical = key.canonicalize().ok();
    let key = toml::Value::String(
        canonical
            .as_deref()
            .unwrap_or(key)
            .to_string_lossy()
            .into_owned(),
    );
    Some(format!(
        "run `codex` once in `{}` and answer its trust prompt, or add `[projects.{key}]` with `trust_level = \"trusted\"` to `{}`",
        cwd.display(),
        config.display(),
    ))
}
