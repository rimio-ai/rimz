//! Merge the provider skill root and RimZ skill chain into a read-only launch view.
//! Unlisted skills that cannot be prepared are omitted and reported.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::agents::ManualSkill;
use crate::agents::skills::{SkillChain, SkillErr};

use super::{DirEntry, DirEntryKind, DirView, SandboxErr, SkillInputs, SkippedSkill, rewrite};

#[derive(Default)]
pub(super) struct SkillView {
    pub dir: Option<DirView>,
    pub skipped: Vec<SkippedSkill>,
    pub copies: Vec<rewrite::PlannedCopy>,
}

#[derive(Default)]
struct ProviderEntries {
    entries: BTreeMap<String, DirEntry>,
    owned: BTreeMap<String, PathBuf>,
}

pub(super) fn plan(
    env: &BTreeMap<String, String>,
    skills_dir: &Path,
    inputs: &SkillInputs<'_>,
) -> Result<SkillView, SandboxErr> {
    match plan_view(env, skills_dir, inputs) {
        Err(error) if inputs.callable.is_none() => {
            tracing::debug!(%error, "keeping native skill discovery because the optional view is unavailable");
            Ok(SkillView::default())
        }
        result => result,
    }
}

fn plan_view(
    env: &BTreeMap<String, String>,
    skills_dir: &Path,
    inputs: &SkillInputs<'_>,
) -> Result<SkillView, SandboxErr> {
    let Some(home) = &inputs.home else {
        return if inputs.callable.is_some() {
            Err(SandboxErr::SkillsNeedRoot {
                kind: inputs.kind.to_owned(),
            })
        } else {
            Ok(SkillView::default())
        };
    };
    super::validate_path(home)?;
    if (inputs.callable.is_some() || inputs.withheld.is_some())
        && inputs.manual == ManualSkill::Unsupported
    {
        return Err(SandboxErr::ManualSkillsUnsupported {
            kind: inputs.kind.to_owned(),
        });
    }
    let chain = SkillChain::from_env(env);
    let ProviderEntries {
        mut entries,
        mut owned,
    } = list_dir(home, chain.as_ref())?;
    let mut roots = vec![home.clone()];
    let mut changed = false;
    let mut skipped = Vec::new();
    let mut copies = Vec::new();
    let mut conflicts = BTreeMap::new();
    if let Some(chain) = &chain {
        for root in chain.roots() {
            super::validate_path(&root)?;
            roots.push(root);
        }
        let inventory = chain.inventory(|name| !entries.contains_key(name))?;
        conflicts = inventory.conflicts;
        for (name, skill) in inventory.skills {
            if let std::collections::btree_map::Entry::Vacant(entry) = entries.entry(name.clone()) {
                let kind = match owned.remove(&name) {
                    Some(target)
                        if crate::utils::path::normalize_path_lexical(&home.join(&target))
                            == skill.path =>
                    {
                        DirEntryKind::Symlink { target }
                    }
                    _ => {
                        changed = true;
                        DirEntryKind::Bind
                    }
                };
                entry.insert(DirEntry {
                    name,
                    source: skill.source,
                    kind,
                });
            }
        }
    }
    changed |= !owned.is_empty();
    let mut shadows = BTreeMap::new();
    if let Some(callable) = inputs.callable {
        crate::config::validate_skill_list(callable)?;
        for name in callable {
            if !entries.contains_key(name.as_str()) {
                if let Some(conflict) = conflicts.get(name.as_str()) {
                    return Err(SkillErr::Conflict {
                        message: conflict.message.clone(),
                    }
                    .into());
                }
                return Err(SandboxErr::UnknownSkill {
                    name: name.to_string(),
                    roots,
                });
            }
        }
    }
    if inputs.callable.is_some() || inputs.withheld.is_some() {
        let mut policies = BTreeMap::new();
        for (name, entry) in &entries {
            if !entry.source.is_dir() || !entry.source.join("SKILL.md").is_file() {
                continue;
            }
            let listed = crate::config::skill_is_callable(inputs.callable, name)
                && inputs.withheld != Some(name.as_str());
            if let Some((other_name, other_listed)) = policies.get(&entry.source) {
                if listed != *other_listed {
                    return Err(SandboxErr::ConflictingSkillAliases {
                        listed: if listed { name } else { other_name }.clone(),
                        unlisted: if listed { other_name } else { name }.clone(),
                        path: entry.source.clone(),
                    });
                }
                continue;
            }
            policies.insert(entry.source.clone(), (name.clone(), listed));
        }
        for (name, entry) in &entries {
            if !matches!(policies.get(&entry.source), Some((_, false)))
                || shadows.contains_key(&entry.source)
            {
                continue;
            }
            match rewrite::plan(skills_dir, &entry.source, inputs.manual) {
                Ok(copy) => {
                    shadows.insert(entry.source.clone(), copy.target.clone());
                    copies.push(copy);
                }
                Err(rewrite::MaterializeErr::Unusable { path, reason }) => {
                    tracing::debug!(skill = %name, path = %path.display(), "omitting skill the view cannot prepare");
                    skipped.push(SkippedSkill {
                        name: name.clone(),
                        path,
                        reason,
                    });
                }
                Err(rewrite::MaterializeErr::Sandbox(error)) => return Err(error),
            }
            changed = true;
        }
    }
    if !changed {
        return Ok(SkillView::default());
    }
    for skill in &skipped {
        entries.remove(&skill.name);
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
    super::validate_path(&root)?;
    for entry in entries.values_mut() {
        super::validate_path(&entry.source)?;
        if matches!(entry.kind, DirEntryKind::Bind)
            && let Some(copy) = shadows.get(&entry.source)
        {
            entry.source = copy.clone();
        }
    }
    shadows.retain(|target, _| {
        entries.values().any(|entry| {
            matches!(entry.kind, DirEntryKind::Symlink { .. }) && entry.source == *target
        }) && !entries
            .values()
            .any(|entry| root.join(&entry.name) == *target)
    });
    Ok(SkillView {
        dir: Some(DirView {
            root,
            entries: entries.into_values().collect(),
            shadows,
        }),
        skipped,
        copies,
    })
}

fn list_dir(root: &Path, chain: Option<&SkillChain>) -> Result<ProviderEntries, SandboxErr> {
    let listing = match std::fs::read_dir(root) {
        Ok(listing) => listing,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ProviderEntries::default());
        }
        Err(source) => {
            return Err(SandboxErr::Io {
                path: root.to_path_buf(),
                source,
            });
        }
    };
    let mut entries = BTreeMap::new();
    let mut owned = BTreeMap::new();
    for entry in listing {
        let entry = entry.map_err(|source| SandboxErr::Io {
            path: root.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| SandboxErr::InvalidPath(path.clone()))?;
        if let Some(chain) = chain
            && let Some(target) = chain.owned_link(&path)?
        {
            if target.to_str().is_none() {
                return Err(SandboxErr::InvalidPath(path));
            }
            owned.insert(name, target);
            continue;
        }
        let source = match path.canonicalize() {
            Ok(source) => source,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(path = %path.display(), "skipping broken skill symlink");
                continue;
            }
            Err(source) => return Err(SandboxErr::Io { path, source }),
        };
        let kind = if path.is_symlink() {
            let target = std::fs::read_link(&path).map_err(|source| SandboxErr::Io {
                path: path.clone(),
                source,
            })?;
            if target.to_str().is_none() {
                return Err(SandboxErr::InvalidPath(path));
            }
            DirEntryKind::Symlink { target }
        } else {
            DirEntryKind::Bind
        };
        entries.insert(name.clone(), DirEntry { name, source, kind });
    }
    Ok(ProviderEntries { entries, owned })
}
