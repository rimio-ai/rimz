//! Name claims shared by the definition trees, traits, and kind bases.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::packs::{
    self, ClaimResolution, ItemFate, ItemType, PackClaim, PackItem, PackName, ResolvedPacks,
};

use super::{CommandsConfig, DefinitionCause, DefinitionErr, LoadedDefinitions, SkillCatalog};

pub(super) struct Item {
    pub name: String,
    pub kind: ItemType,
    pub path: PathBuf,
    pub pack: Option<PackName>,
    pub fate: Option<ItemFate>,
}

impl Item {
    pub fn selected(&self) -> bool {
        matches!(self.fate, None | Some(ItemFate::Loads))
    }

    pub fn excluded(&self) -> bool {
        matches!(self.fate, Some(ItemFate::Excluded | ItemFate::ExcludedType))
    }

    pub fn error(&self, message: impl std::fmt::Display) -> DefinitionErr {
        let mut error = DefinitionErr::new(&self.path, message.to_string());
        if self.pack.is_some() {
            error.cause = DefinitionCause::PackProblem;
            error.message.push_str(&self.exclusion_fix());
        }
        error
    }

    /// The `; run ...` tail naming the command that excludes this pack item;
    /// empty for a home item.
    pub(super) fn exclusion_fix(&self) -> String {
        let Some(pack) = &self.pack else {
            return String::new();
        };
        let target = format!("{pack}:{}", self.name);
        // YAML can carry NUL, which argv cannot; exclude the type in that case.
        let target = shlex::try_quote(&target).unwrap_or_else(|_| pack.as_str().into());
        format!(
            "; run `rimz packs exclude {target} --type {}`",
            self.kind.as_str()
        )
    }

    pub(super) fn namespace(&self) -> &str {
        if kind_base(self.kind, &self.path, &self.name) {
            return "bases";
        }
        match self.kind {
            ItemType::Agents | ItemType::Subagents | ItemType::Teams => "definitions",
            _ => self.kind.as_str(),
        }
    }
}

pub(super) fn kind_base(kind: ItemType, path: &Path, name: &str) -> bool {
    kind == ItemType::Agents
        && path.file_stem().is_some_and(|stem| stem == name)
        && crate::agents::find_definition(name).is_some()
}

pub(super) struct Catalog {
    pub items: Vec<Item>,
    pub claims: BTreeMap<String, BTreeMap<String, ClaimResolution>>,
}

impl Catalog {
    pub fn new(
        home: &Path,
        packs: &ResolvedPacks,
        commands: &CommandsConfig,
        config_path: &Path,
        texts: &SkillCatalog<'_>,
        loaded: &mut LoadedDefinitions,
    ) -> Self {
        let mut items = Vec::new();
        for (root, pack) in std::iter::once((home, None)).chain(
            packs
                .packs
                .iter()
                .map(|pack| (pack.root.as_path(), Some(pack))),
        ) {
            for kind in [
                ItemType::Agents,
                ItemType::Subagents,
                ItemType::Teams,
                ItemType::Traits,
            ] {
                let paths = match super::files(root, kind.as_str()) {
                    Ok(paths) => paths,
                    Err(mut error) => {
                        if let Some(pack) = pack {
                            error.cause = DefinitionCause::PackProblem;
                            loaded
                                .pack_problems
                                .entry(pack.name.clone())
                                .or_default()
                                .push(packs::Problem {
                                    path: error.path.clone(),
                                    message: error.message.clone(),
                                });
                        }
                        loaded.errors.push(error);
                        continue;
                    }
                };
                for path in paths {
                    if pack.is_some() && kind == ItemType::Traits && super::reserved(root, &path) {
                        continue;
                    }
                    let stem = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let name = if kind == ItemType::Traits
                        || (kind == ItemType::Agents
                            && crate::agents::find_definition(&stem).is_some())
                    {
                        stem
                    } else {
                        #[derive(serde::Deserialize)]
                        struct Name {
                            name: Option<String>,
                        }
                        texts
                            .read_text(&path)
                            .ok()
                            .and_then(|text| {
                                let (yaml, _) = super::frontmatter::split(&path, &text).ok()?;
                                serde_saphyr::from_str::<Name>(yaml).ok()?.name
                            })
                            .unwrap_or(stem)
                    };
                    let fate = pack.map(|pack| {
                        if pack.excluded.types.contains(&kind) {
                            ItemFate::ExcludedType
                        } else if pack.excluded.contains(kind, &name) {
                            ItemFate::Excluded
                        } else if kind == ItemType::Teams && commands.0.contains_key(&name) {
                            ItemFate::Overridden {
                                path: config_path.to_owned(),
                            }
                        } else {
                            ItemFate::Loads
                        }
                    });
                    items.push(Item {
                        name,
                        kind,
                        path,
                        pack: pack.map(|pack| pack.name.clone()),
                        fate,
                    });
                }
            }
        }
        let mut claims = BTreeMap::new();
        for namespace in ["definitions", "bases", "traits"] {
            let members: Vec<_> = items
                .iter()
                .filter(|item| {
                    item.namespace() == namespace
                        && !matches!(item.fate, Some(ItemFate::Overridden { .. }))
                })
                .collect();
            let resolved = packs::resolve_claims(
                members
                    .iter()
                    .filter(|item| item.pack.is_none())
                    .map(|item| (item.name.clone(), item.path.clone())),
                members.iter().filter_map(|item| {
                    Some(PackClaim {
                        name: item.name.clone(),
                        kind: item.kind,
                        pack: item.pack.clone()?,
                        path: item.path.clone(),
                        excluded: item.excluded(),
                    })
                }),
            );
            for (name, claim) in &resolved {
                if let ClaimResolution::Conflict { claims, message } = claim {
                    let mut error = DefinitionErr::new(&claims[0].path, message);
                    if namespace == "traits" {
                        error.cause = DefinitionCause::PackProblem;
                    } else {
                        loaded
                            .failed
                            .entry(name.clone())
                            .or_default()
                            .extend(claims.iter().map(|claim| claim.path.clone()));
                    }
                    loaded.errors.push(error);
                    for claim in claims.iter().filter(|claim| claim.kind == ItemType::Teams) {
                        if let Ok(text) = texts.read_text(&claim.path)
                            && let Ok((yaml, _)) = super::frontmatter::split(&claim.path, &text)
                            && let Ok(team) = super::frontmatter::parse::<
                                super::frontmatter::TeamFrontmatter,
                            >(&claim.path, yaml)
                        {
                            for role in team.roles {
                                let handle = role.role.as_deref().unwrap_or(&role.agent);
                                loaded
                                    .failed
                                    .entry(format!("{name}.{handle}"))
                                    .or_default()
                                    .extend(claims.iter().map(|claim| claim.path.clone()));
                            }
                        }
                    }
                }
            }
            claims.insert(namespace.to_owned(), resolved);
        }
        for item in &mut items {
            if item.pack.is_none()
                || item.excluded()
                || matches!(item.fate, Some(ItemFate::Overridden { .. }))
            {
                continue;
            }
            item.fate = Some(match &claims[item.namespace()][&item.name] {
                ClaimResolution::Home(path) => ItemFate::Overridden { path: path.clone() },
                ClaimResolution::Pack(_) => ItemFate::Loads,
                ClaimResolution::Conflict { claims, message } => ItemFate::Conflict {
                    claims: claims.clone(),
                    message: message.clone(),
                },
            });
        }
        Self { items, claims }
    }

    pub fn pack_items(
        &self,
        loaded: &LoadedDefinitions,
        texts: &SkillCatalog<'_>,
    ) -> Vec<PackItem> {
        self.items
            .iter()
            .filter_map(|item| {
                let mut fate = item.fate.clone()?;
                if fate == ItemFate::Loads {
                    if let Some(error) = loaded.errors.iter().find(|error| error.path == item.path)
                    {
                        fate = ItemFate::Failed {
                            message: error.message.clone(),
                        };
                    } else if item.kind == ItemType::Traits
                        && let Err(error) = texts.read_text(&item.path)
                    {
                        fate = ItemFate::Failed {
                            message: error.to_string(),
                        };
                    }
                }
                Some(PackItem {
                    pack: item.pack.clone()?,
                    kind: item.kind,
                    name: item.name.clone(),
                    path: item.path.clone(),
                    fate,
                })
            })
            .collect()
    }
}
