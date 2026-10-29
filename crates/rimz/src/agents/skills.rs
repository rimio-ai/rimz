//! Host skill discovery and provider-owned per-launch skill and permission settings.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::config::SkillName;
use crate::packs::{self, ClaimResolution, ItemType, PackClaim, PackName, ResolvedPack};
use crate::utils::path::normalize_path_lexical;

#[derive(Clone, Debug)]
pub struct SkillChain {
    library: PathBuf,
    packs_dir: PathBuf,
    packs: Vec<ResolvedPack>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PackSkillFate {
    Loads,
    Overridden(PathBuf),
    Excluded,
    ExcludedType,
    Conflict {
        claims: Vec<PackClaim>,
        message: String,
    },
}

#[derive(Clone, Debug)]
pub struct PackSkill {
    pub pack: PackName,
    pub name: String,
    pub path: PathBuf,
    pub fate: PackSkillFate,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct SkillInventory {
    pub(crate) skills: BTreeMap<String, SkillDir>,
    pub(crate) conflicts: BTreeMap<String, packs::Problem>,
    pack_skills: Vec<PackSkill>,
}

impl SkillChain {
    pub fn from_home(home: &Path) -> Self {
        let home = normalize_path_lexical(home);
        Self {
            library: crate::disk::paths::skills_library_under(&home),
            packs_dir: crate::disk::paths::packs_dir(&home),
            packs: packs::resolve(&home).packs,
        }
    }

    pub(crate) fn from_env(env: &BTreeMap<String, String>) -> Option<Self> {
        Some(Self {
            library: normalize_path_lexical(&crate::disk::paths::skills_library_in(env)?),
            packs_dir: normalize_path_lexical(&crate::disk::paths::packs_dir_in(env)?),
            packs: packs::resolve_in(env).packs,
        })
    }

    pub(crate) fn roots(&self) -> Vec<PathBuf> {
        std::iter::once(self.library.clone())
            .chain(
                self.packs
                    .iter()
                    .map(|pack| crate::disk::paths::skills_library_under(&pack.root)),
            )
            .collect()
    }

    pub fn pack_skills(&self) -> Result<Vec<PackSkill>, SkillErr> {
        Ok(self.inventory(|_| true)?.pack_skills)
    }

    pub(super) fn owned_roots(&self) -> [&Path; 2] {
        [&self.library, &self.packs_dir]
    }

    pub(crate) fn owned_link(&self, path: &Path) -> Result<Option<PathBuf>, SkillErr> {
        match std::fs::read_link(path) {
            Ok(target) => Ok(super::skill_links::owned_target(
                path.parent().unwrap_or(path),
                self.owned_roots(),
                &target,
            )
            .map(|_| target)),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::InvalidInput
                ) =>
            {
                Ok(None)
            }
            Err(source) => Err(SkillErr::Io {
                path: path.to_owned(),
                source,
            }),
        }
    }

    pub(crate) fn inventory(
        &self,
        keep: impl Fn(&str) -> bool,
    ) -> Result<SkillInventory, SkillErr> {
        let home = scan(&self.library, None, &keep, true)?;
        let mut shipped = Vec::new();
        for pack in &self.packs {
            let root = crate::disk::paths::skills_library_under(&pack.root);
            for (name, skill) in scan(&root, None, &keep, true)? {
                shipped.push((pack, name, skill));
            }
        }
        let claims = packs::resolve_claims(
            home.iter()
                .map(|(name, skill)| (name.clone(), skill.path.join("SKILL.md"))),
            shipped.iter().map(|(pack, name, skill)| PackClaim {
                name: name.clone(),
                pack: pack.name.clone(),
                path: skill.path.join("SKILL.md"),
                excluded: pack.excluded.contains(ItemType::Skills, name),
            }),
        );
        let mut inventory = SkillInventory {
            skills: home,
            ..SkillInventory::default()
        };
        for (pack, name, skill) in shipped {
            let fate = if pack.excluded.types.contains(&ItemType::Skills) {
                PackSkillFate::ExcludedType
            } else if pack.excluded.contains(ItemType::Skills, &name) {
                PackSkillFate::Excluded
            } else {
                match &claims[&name] {
                    ClaimResolution::Home(path) => PackSkillFate::Overridden(path.clone()),
                    ClaimResolution::Pack(_) => {
                        inventory.skills.insert(name.clone(), skill.clone());
                        PackSkillFate::Loads
                    }
                    ClaimResolution::Conflict { claims, message } => {
                        inventory
                            .conflicts
                            .entry(name.clone())
                            .or_insert_with(|| packs::Problem {
                                path: skill.path.join("SKILL.md"),
                                message: message.clone(),
                            });
                        PackSkillFate::Conflict {
                            claims: claims.clone(),
                            message: message.clone(),
                        }
                    }
                }
            };
            inventory.pack_skills.push(PackSkill {
                pack: pack.name.clone(),
                name,
                path: skill.path,
                fate,
            });
        }
        Ok(inventory)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ProviderSkillKey(String);

impl ProviderSkillKey {
    pub(super) fn new(name: String) -> Self {
        Self(name)
    }

    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct SkillDir {
    pub(crate) name: String,
    pub(crate) source: PathBuf,
    pub(crate) path: PathBuf,
}

#[derive(Clone, Copy, Debug)]
pub enum HostSkills {
    Unsupported,
    Switch {
        flag: &'static str,
        effect: &'static str,
        key: fn(&SkillDir) -> Result<ProviderSkillKey, LaunchSettingsErr>,
        render: RenderHostSkills,
    },
}

#[derive(Clone, Copy, Debug)]
pub enum ToolRules {
    Unsupported,
    Settings { render: RenderToolRules },
}

type RenderToolRules = fn(
    &[crate::config::ToolRule],
    (&Path, &Path),
    &mut Vec<String>,
    &mut Option<LaunchSettingsArtifact>,
) -> Result<(), LaunchSettingsErr>;

/// Publication path, settings, and original source path for chained-merge diagnostics.
pub(crate) type LaunchSettingsArtifact = (PathBuf, serde_json::Value, PathBuf);

type RenderHostSkills = fn(
    &[ProviderSkillKey],
    &Path,
    &Path,
    &mut Vec<String>,
) -> Result<Option<LaunchSettingsArtifact>, LaunchSettingsErr>;

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
    #[error("{message}")]
    Conflict { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum LaunchSettingsErr {
    #[error(transparent)]
    Skills(#[from] SkillErr),
    #[error("{0}; correct skill root access, or set allow-routine-rimz = false")]
    RoutineSkills(#[source] SkillErr),
    #[error("invalid launch settings at {path}: {reason}")]
    Settings { path: PathBuf, reason: String },
    #[error("skills sharing provider name {name:?} have conflicting invocation policies; list all of their directories or none", name = .key.as_str())]
    ConflictingKey { key: ProviderSkillKey },
}

pub(crate) fn enumerate(
    provider_root: Option<&Path>,
    chain: Option<&SkillChain>,
    keep: fn(&str) -> bool,
) -> Result<SkillInventory, SkillErr> {
    let provider = provider_root.map_or_else(
        || Ok(BTreeMap::new()),
        |root| scan(root, chain, keep, false),
    )?;
    let mut inventory = chain.map_or_else(
        || Ok(SkillInventory::default()),
        |chain| chain.inventory(|name| keep(name) && !provider.contains_key(name)),
    )?;
    inventory.skills.extend(provider);
    Ok(inventory)
}

fn scan(
    root: &Path,
    chain: Option<&SkillChain>,
    keep: impl Fn(&str) -> bool,
    require_file: bool,
) -> Result<BTreeMap<String, SkillDir>, SkillErr> {
    let mut skills = BTreeMap::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(skills),
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
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !keep(&name) {
            continue;
        }
        let path = entry.path();
        if let Some(chain) = chain
            && chain.owned_link(&path)?.is_some()
        {
            continue;
        }
        if !path.is_dir() {
            continue;
        }
        let marker = path.join("SKILL.md");
        match marker.metadata() {
            Ok(metadata) if !require_file || metadata.is_file() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(SkillErr::Io {
                    path: marker,
                    source,
                });
            }
        }
        let source = path.canonicalize().map_err(|source| SkillErr::Io {
            path: path.clone(),
            source,
        })?;
        skills.insert(name.clone(), SkillDir { name, source, path });
    }
    Ok(skills)
}

impl HostSkills {
    pub(crate) fn apply(
        self,
        (listed, withheld): (Option<&[SkillName]>, Option<&str>),
        root: Option<&Path>,
        chain: Option<&SkillChain>,
        paths: (&Path, &Path),
        args: &mut Vec<String>,
    ) -> Result<(HostSkillPlan, Option<LaunchSettingsArtifact>), LaunchSettingsErr> {
        let Self::Switch {
            flag,
            effect,
            key,
            render,
        } = self
        else {
            return Ok((HostSkillPlan::Unenforced, None));
        };
        let inventory = enumerate(root, chain, |_| true)?;
        for name in listed.into_iter().flatten() {
            if let Some(conflict) = inventory.conflicts.get(name.as_str()) {
                return Err(SkillErr::Conflict {
                    message: conflict.message.clone(),
                }
                .into());
            }
            if !inventory.skills.contains_key(name.as_str()) {
                return Err(SkillErr::UnknownSkill {
                    name: name.clone(),
                    roots: root
                        .into_iter()
                        .map(Path::to_owned)
                        .chain(chain.into_iter().flat_map(SkillChain::roots))
                        .collect(),
                }
                .into());
            }
        }
        let mut callable = Vec::new();
        let mut unlisted = Vec::new();
        for (name, skill) in inventory.skills {
            let provider_key = key(&skill)?;
            let (keys, other_policy) = if crate::config::skill_is_callable(listed, &name)
                && withheld != Some(name.as_str())
            {
                (&mut callable, &unlisted)
            } else {
                (&mut unlisted, &callable)
            };
            if other_policy.contains(&provider_key) {
                return Err(LaunchSettingsErr::ConflictingKey { key: provider_key });
            }
            keys.push(provider_key);
        }
        let artifact = if listed.is_none() && unlisted.is_empty() {
            None
        } else {
            render(&unlisted, paths.0, paths.1, args)?
        };
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
        let library = temp.path().join("rimz/skills");
        let foreign = temp.path().join("foreign");
        for path in [
            provider.join("same"),
            library.join("same"),
            library.join("extra"),
            foreign.clone(),
        ] {
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join("SKILL.md"), "skill").unwrap();
        }
        std::os::unix::fs::symlink(provider.join("missing"), provider.join("broken")).unwrap();
        std::os::unix::fs::symlink(&foreign, provider.join("alias")).unwrap();
        std::os::unix::fs::symlink(library.join("extra"), provider.join("owned")).unwrap();
        std::fs::create_dir(provider.join("not-a-skill")).unwrap();
        let chain = SkillChain::from_home(library.parent().unwrap());
        let skills = enumerate(Some(&provider), Some(&chain), |_| true)
            .unwrap()
            .skills;
        assert_eq!(skills.len(), 3);
        assert_eq!(
            skills["same"].source,
            provider.join("same").canonicalize().unwrap()
        );
        assert_eq!(skills["alias"].source, foreign.canonicalize().unwrap());
    }

    #[test]
    fn discovery_does_not_inspect_shadowed_rimz_skills() {
        let temp = tempfile::tempdir().unwrap();
        let provider = temp.path().join("provider");
        let chain = SkillChain::from_home(temp.path());
        std::fs::create_dir_all(provider.join("same")).unwrap();
        std::fs::write(provider.join("same/SKILL.md"), "provider").unwrap();
        std::fs::create_dir_all(chain.library.join("same")).unwrap();
        std::os::unix::fs::symlink("SKILL.md", chain.library.join("same/SKILL.md")).unwrap();
        let result = enumerate(Some(&provider), Some(&chain), |_| true);
        assert!(
            result.is_ok(),
            "the provider's copy wins without reading the lower marker: {result:?}"
        );
    }

    #[test]
    fn discovery_loads_pack_skills_and_withholds_conflicts_and_exclusions() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("rimz");
        let library = SkillChain::from_home(&home).library;
        for pack in ["one", "two"] {
            let source = temp.path().join(pack);
            std::fs::create_dir_all(&source).unwrap();
            std::fs::write(
                source.join("rimz-packs.toml"),
                format!("[[pack]]\nname = '{pack}'\ndescription = 'Skills'\npath = '.'\n"),
            )
            .unwrap();
            for name in [pack, "conflict", "hidden", "excluded"] {
                let path = source.join(ItemType::Skills.as_str()).join(name);
                std::fs::create_dir_all(&path).unwrap();
                std::fs::write(path.join("SKILL.md"), "skill").unwrap();
            }
            crate::packs::install(&home, &source).unwrap();
            crate::packs::exclude(
                &home,
                &pack.parse().unwrap(),
                &crate::packs::ExclusionTarget::Item(crate::packs::ItemKey {
                    kind: crate::packs::ItemType::Skills,
                    name: "excluded".to_owned(),
                }),
            )
            .unwrap();
        }
        std::fs::create_dir_all(library.join("hidden")).unwrap();
        std::fs::write(library.join("hidden/SKILL.md"), "home").unwrap();
        let chain = SkillChain::from_home(&home);
        let skills = enumerate(None, Some(&chain), |_| true).unwrap().skills;
        assert_eq!(
            skills.keys().map(String::as_str).collect::<Vec<_>>(),
            ["hidden", "one", "two"]
        );
    }
}
