//! Name claims shared by the definition trees, traits, and kind bases.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::packs::{self, ClaimResolution, ItemType, PackClaim, PackName, ResolvedPacks};

use super::{
    DefinitionCause, DefinitionErr, LoadedDefinitions, PackDefinition, PackDefinitionFate,
    SkillCatalog,
};

pub(super) struct Item {
    pub name: String,
    pub kind: ItemType,
    pub path: PathBuf,
    pub pack: Option<PackName>,
    pub fate: Option<PackDefinitionFate>,
}

impl Item {
    pub fn selected(&self) -> bool {
        matches!(self.fate, None | Some(PackDefinitionFate::Loads))
    }

    pub fn excluded(&self) -> bool {
        matches!(
            self.fate,
            Some(PackDefinitionFate::Excluded | PackDefinitionFate::ExcludedType)
        )
    }

    fn namespace(&self) -> &str {
        if self.kind == ItemType::Agents
            && self
                .path
                .file_stem()
                .is_some_and(|stem| stem == self.name.as_str())
            && crate::agents::find_definition(&self.name).is_some()
        {
            return "bases";
        }
        match self.kind {
            ItemType::Agents | ItemType::Subagents => "definitions",
            _ => self.kind.as_str(),
        }
    }
}

pub(super) struct Catalog {
    pub items: Vec<Item>,
    pub claims: BTreeMap<String, BTreeMap<String, ClaimResolution>>,
}

impl Catalog {
    pub fn new(
        home: &Path,
        packs: &ResolvedPacks,
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
                        if pack.is_some() {
                            error.cause = DefinitionCause::PackProblem;
                        }
                        loaded.errors.push(error);
                        continue;
                    }
                };
                for path in paths {
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
                            PackDefinitionFate::ExcludedType
                        } else if pack.excluded.contains(kind, &name) {
                            PackDefinitionFate::Excluded
                        } else {
                            PackDefinitionFate::Loads
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
        for namespace in ["definitions", "bases", "teams", "traits"] {
            let members: Vec<_> = items
                .iter()
                .filter(|item| item.namespace() == namespace)
                .collect();
            let resolved = packs::resolve_claims(
                members
                    .iter()
                    .filter(|item| item.pack.is_none())
                    .map(|item| (item.name.clone(), item.path.clone())),
                members.iter().filter_map(|item| {
                    Some(PackClaim {
                        name: item.name.clone(),
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
                    if namespace == "teams" {
                        for claim in claims {
                            if let Ok(text) = texts.read_text(&claim.path)
                                && let Ok((yaml, _)) = super::frontmatter::split(&claim.path, &text)
                                && let Ok(team) = super::frontmatter::parse::<
                                    super::frontmatter::TeamFrontmatter,
                                >(
                                    &claim.path, yaml
                                )
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
            }
            claims.insert(namespace.to_owned(), resolved);
        }
        for item in &mut items {
            if item.pack.is_none() || item.excluded() {
                continue;
            }
            item.fate = Some(match &claims[item.namespace()][&item.name] {
                ClaimResolution::Home(path) => PackDefinitionFate::Overridden(path.clone()),
                ClaimResolution::Pack(_) => PackDefinitionFate::Loads,
                ClaimResolution::Conflict { claims, message } => PackDefinitionFate::Conflict {
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
    ) -> Vec<PackDefinition> {
        self.items
            .iter()
            .filter_map(|item| {
                let mut fate = item.fate.clone()?;
                if fate == PackDefinitionFate::Loads {
                    if let Some(error) = loaded.errors.iter().find(|error| error.path == item.path)
                    {
                        fate = PackDefinitionFate::Failed(error.message.clone());
                    } else if item.kind == ItemType::Traits
                        && let Err(error) = texts.read_text(&item.path)
                    {
                        fate = PackDefinitionFate::Failed(error.to_string());
                    }
                }
                Some(PackDefinition {
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
