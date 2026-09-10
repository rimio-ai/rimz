//! Merge the provider skill root and RimZ library into a read-only launch view.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::ManualSkill;

use super::{DirEntry, DirView, SandboxErr, SkillInputs, rewrite};

pub(super) fn prepare(
    env: &BTreeMap<String, String>,
    skills_dir: &Path,
    inputs: &SkillInputs<'_>,
) -> Result<Option<DirView>, SandboxErr> {
    let Some(home) = &inputs.home else {
        return if inputs.callable.is_some() {
            Err(SandboxErr::SkillsNeedRoot {
                kind: inputs.kind.to_owned(),
            })
        } else {
            Ok(None)
        };
    };
    super::validate_path(home)?;
    if inputs.callable.is_some() && inputs.manual == ManualSkill::Unsupported {
        return Err(SandboxErr::ManualSkillsUnsupported {
            kind: inputs.kind.to_owned(),
        });
    }
    let mut entries = list_dir(home)?;
    let mut roots = vec![home.clone()];
    let mut changed = false;
    let config = env
        .get("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env.get("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| Path::new(home).join(".config"))
        });
    if let Some(config) = config {
        let library = config.join("rimz/skills");
        super::validate_path(&library)?;
        for (name, source) in list_dir(&library)? {
            if let std::collections::btree_map::Entry::Vacant(entry) = entries.entry(name) {
                entry.insert(source);
                changed = true;
            }
        }
        roots.push(library);
    }
    if let Some(callable) = inputs.callable {
        crate::config::validate_skill_list(callable)?;
        for name in callable {
            if !entries.contains_key(name.as_str()) {
                return Err(SandboxErr::UnknownSkill {
                    name: name.to_string(),
                    roots,
                });
            }
        }
        for (name, source) in &mut entries {
            if callable.iter().any(|skill| skill.as_str() == name) || !source.is_dir() {
                continue;
            }
            *source = rewrite::materialize(skills_dir, source, inputs.manual)?;
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }
    let root = match home.canonicalize() {
        Ok(root) => root,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            crate::utils::path::normalize_path_lexical(home)
        }
        Err(source) => {
            return Err(SandboxErr::Io {
                path: home.clone(),
                source,
            });
        }
    };
    Ok(Some(DirView {
        root,
        entries: entries
            .into_iter()
            .map(|(name, source)| DirEntry { name, source })
            .collect(),
    }))
}

fn list_dir(root: &Path) -> Result<BTreeMap<String, PathBuf>, SandboxErr> {
    let listing = match std::fs::read_dir(root) {
        Ok(listing) => listing,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(source) => {
            return Err(SandboxErr::Io {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    let mut entries = BTreeMap::new();
    for entry in listing {
        let entry = entry.map_err(|source| SandboxErr::Io {
            path: root.to_path_buf(),
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
        entries.insert(name, source);
    }
    Ok(entries)
}
