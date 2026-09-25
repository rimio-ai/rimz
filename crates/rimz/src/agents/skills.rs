//! Host skill discovery and provider-owned per-launch restrictions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::SkillName;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProviderSkillKey(String);

impl ProviderSkillKey {
    pub(crate) fn new(name: String) -> Self {
        Self(name)
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct SkillDir {
    pub(crate) name: String,
    pub(crate) source: PathBuf,
}

#[derive(Clone, Copy, Debug)]
pub enum HostSkills {
    Unsupported,
    Switch {
        flag: &'static str,
        effect: &'static str,
        key: fn(&SkillDir) -> Result<ProviderSkillKey, HostSkillArgErr>,
        render: RenderHostSkills,
    },
}

pub(crate) type HostSkillArtifact = (PathBuf, serde_json::Value);

type RenderHostSkills = fn(
    &[ProviderSkillKey],
    &Path,
    &Path,
    &mut Vec<String>,
) -> Result<Option<HostSkillArtifact>, HostSkillArgErr>;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum HostSkillPlan {
    Applied {
        flag: &'static str,
        effect: &'static str,
        listed: Vec<ProviderSkillKey>,
        unlisted: Vec<ProviderSkillKey>,
    },
    Unenforced,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillErr {
    #[error("reading skills at {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "unknown skill '{name}'; searched {roots:?}; install it in one of these roots or remove it from skills"
    )]
    UnknownSkill {
        name: SkillName,
        roots: Vec<PathBuf>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum HostSkillArgErr {
    #[error(transparent)]
    Skills(#[from] SkillErr),
    #[error("invalid host skill settings at {path}: {reason}")]
    Settings { path: PathBuf, reason: String },
    #[error("skills sharing provider name {name:?} have conflicting invocation policies; list all of their directories or none", name = .key.as_str())]
    ConflictingKey { key: ProviderSkillKey },
}

pub(crate) fn enumerate(
    provider_root: Option<&Path>,
    library: Option<&Path>,
) -> Result<BTreeMap<String, SkillDir>, SkillErr> {
    let mut skills = BTreeMap::new();
    for root in provider_root.into_iter().chain(library) {
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(SkillErr::Io {
                    path: root.to_owned(),
                    source,
                });
            }
        };
        for entry in entries {
            let entry = entry.map_err(|source| SkillErr::Io {
                path: root.to_owned(),
                source,
            })?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let marker = path.join("SKILL.md");
            if !marker.try_exists().map_err(|source| SkillErr::Io {
                path: marker,
                source,
            })? {
                continue;
            }
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if skills.contains_key(&name) {
                continue;
            }
            let source = path
                .canonicalize()
                .map_err(|source| SkillErr::Io { path, source })?;
            skills.insert(name.clone(), SkillDir { name, source });
        }
    }
    Ok(skills)
}

impl HostSkills {
    pub(crate) fn apply(
        self,
        listed: &[SkillName],
        root: Option<&Path>,
        library: Option<&Path>,
        paths: (&Path, &Path),
        args: &mut Vec<String>,
    ) -> Result<(HostSkillPlan, Option<HostSkillArtifact>), HostSkillArgErr> {
        let Self::Switch {
            flag,
            effect,
            key,
            render,
        } = self
        else {
            return Ok((HostSkillPlan::Unenforced, None));
        };
        let skills = enumerate(root, library)?;
        for name in listed {
            if !skills.contains_key(name.as_str()) {
                return Err(SkillErr::UnknownSkill {
                    name: name.clone(),
                    roots: root
                        .into_iter()
                        .chain(library)
                        .map(Path::to_owned)
                        .collect(),
                }
                .into());
            }
        }
        let mut callable = Vec::new();
        let mut unlisted = Vec::new();
        for (name, skill) in skills {
            let provider_key = key(&skill)?;
            let (keys, other_policy) = if listed.iter().any(|listed| listed.as_str() == name) {
                (&mut callable, &unlisted)
            } else {
                (&mut unlisted, &callable)
            };
            if other_policy.contains(&provider_key) {
                return Err(HostSkillArgErr::ConflictingKey { key: provider_key });
            }
            keys.push(provider_key);
        }
        let artifact = render(&unlisted, paths.0, paths.1, args)?;
        Ok((
            HostSkillPlan::Applied {
                flag,
                effect,
                listed: callable,
                unlisted,
            },
            artifact,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_prefers_provider_skips_broken_links_and_records_sources() {
        let temp = tempfile::tempdir().unwrap();
        let provider = temp.path().join("provider");
        let library = temp.path().join("library");
        for path in [
            provider.join("same"),
            library.join("same"),
            library.join("extra"),
        ] {
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("SKILL.md"), "skill").unwrap();
        }
        std::os::unix::fs::symlink(provider.join("missing"), provider.join("broken")).unwrap();
        std::os::unix::fs::symlink(library.join("extra"), provider.join("alias")).unwrap();
        std::fs::create_dir(provider.join("not-a-skill")).unwrap();
        let skills = enumerate(Some(&provider), Some(&library)).unwrap();
        assert_eq!(skills.len(), 3);
        assert_eq!(
            skills["same"].source,
            provider.join("same").canonicalize().unwrap()
        );
        assert_eq!(
            skills["alias"].source,
            library.join("extra").canonicalize().unwrap()
        );
    }
}
