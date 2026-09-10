//! Resolve host skill directories into read-only per-profile views.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::config::{SkillMode, SkillSpec};

use super::{DirEntry, DirView, SandboxErr};

pub(super) fn prepare(
    env: &BTreeMap<String, String>,
    specs: &[SkillSpec],
) -> Result<Vec<DirView>, SandboxErr> {
    crate::config::validate_skill_list(specs)?;
    if specs.is_empty() {
        return Ok(Vec::new());
    }
    let mut roots = Vec::new();
    if let Some(home) = crate::agents::registry::find_definition("claude")
        .and_then(|definition| definition.config_home(env))
    {
        roots.push(home.join("skills"));
    }
    if let Some(home) = env.get("HOME").filter(|value| !value.is_empty()) {
        roots.push(PathBuf::from(home).join(".agents/skills"));
    }
    let mut found = BTreeSet::new();
    let mut views = Vec::new();
    for root in &roots {
        let listing = match std::fs::read_dir(root) {
            Ok(listing) => listing,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(SandboxErr::Io {
                    path: root.clone(),
                    source,
                });
            }
        };
        let mut entries = Vec::new();
        for entry in listing {
            let entry = entry.map_err(|source| SandboxErr::Io {
                path: root.clone(),
                source,
            })?;
            let path = entry.path();
            let source = match path.canonicalize() {
                Ok(source) => source,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    tracing::debug!(path = %path.display(), "skipping broken skill symlink");
                    continue;
                }
                Err(source) => return Err(SandboxErr::Io { path, source }),
            };
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| SandboxErr::InvalidPath(path))?;
            found.insert(name.clone());
            if specs
                .iter()
                .any(|spec| spec.name.as_str() == name && spec.mode == SkillMode::Off)
            {
                continue;
            }
            entries.push(DirEntry { name, source });
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        views.push(DirView {
            root: root.canonicalize().map_err(|source| SandboxErr::Io {
                path: root.clone(),
                source,
            })?,
            entries,
        });
    }
    for spec in specs {
        if !found.contains(spec.name.as_str()) {
            return Err(SandboxErr::UnknownSkill {
                name: spec.name.to_string(),
                roots,
            });
        }
    }
    Ok(views)
}
