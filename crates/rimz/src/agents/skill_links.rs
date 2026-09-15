//! Read-only reconciliation and explicit application of provider-root skill links.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::utils::path::normalize_path_lexical;

pub enum Desired {
    Library,
    None,
}

#[derive(Debug, serde::Serialize)]
pub enum SkillLinkAction {
    Link { name: String, target: PathBuf },
    Unlink { name: String },
}

#[derive(Debug, serde::Serialize)]
pub struct SkillLinkPlan {
    root: PathBuf,
    library: PathBuf,
    actions: Vec<SkillLinkAction>,
    shadowed: Vec<String>,
}

#[derive(Debug, Default)]
pub struct SkillLinkOutcome {
    pub linked: usize,
    pub unlinked: usize,
    pub shadowed: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillLinkErr {
    #[error(
        "cannot reconcile RimZ skill links at {}: {source}; fix access to that path, or empty the RimZ skill library to launch without links",
        path.display()
    )]
    Io { path: PathBuf, source: io::Error },
}

impl SkillLinkPlan {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn library(&self) -> &Path {
        &self.library
    }

    pub fn actions(&self) -> &[SkillLinkAction] {
        &self.actions
    }

    pub fn shadowed(&self) -> &[String] {
        &self.shadowed
    }

    pub fn is_empty(&self) -> bool {
        self.actions.is_empty() && self.shadowed.is_empty()
    }

    pub fn has_owned_changes(&self) -> bool {
        !self.actions.is_empty()
    }

    pub fn shadowed_report(&self, names: &[String]) -> Option<String> {
        (!names.is_empty()).then(|| format!(
            "{} library skill(s) not linked into {}: {} — these entries are yours; move them into {} or delete them",
            names.len(), self.root.display(), names.join(", "), self.library.display()
        ))
    }
}

impl std::fmt::Display for SkillLinkPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let linked = self
            .actions
            .iter()
            .filter(|action| matches!(action, SkillLinkAction::Link { .. }))
            .count();
        let unlinked = self.actions.len() - linked;
        let mut lines = Vec::new();
        if linked > 0 {
            lines.push(format!(
                "linked {linked} skill(s) into {}",
                self.root.display()
            ));
        }
        if unlinked > 0 {
            lines.push(format!(
                "unlinked {unlinked} stale skill link(s) from {}",
                self.root.display()
            ));
        }
        lines.extend(self.shadowed_report(&self.shadowed));
        write!(f, "{}", lines.join("\n"))
    }
}

fn io_err(path: &Path, source: io::Error) -> SkillLinkErr {
    SkillLinkErr::Io {
        path: path.to_owned(),
        source,
    }
}

fn absolute(path: &Path) -> Result<PathBuf, SkillLinkErr> {
    std::path::absolute(path)
        .map(|path| normalize_path_lexical(&path))
        .map_err(|source| io_err(path, source))
}

fn entries(root: &Path) -> Result<BTreeMap<String, PathBuf>, SkillLinkErr> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(err) => return Err(io_err(root, err)),
    };
    let mut named = BTreeMap::new();
    for entry in entries {
        let entry = entry.map_err(|err| io_err(root, err))?;
        // A non-UTF-8 name can never match a skill name, so it is never RimZ's.
        if let Ok(name) = entry.file_name().into_string() {
            named.insert(name, entry.path());
        }
    }
    Ok(named)
}

/// A library entry is a skill when it resolves to a directory holding `SKILL.md`;
/// a broken or looping user symlink is simply not one.
fn is_skill(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|meta| meta.is_dir())
        && fs::metadata(path.join("SKILL.md")).is_ok_and(|meta| meta.is_file())
}

fn owned_target(root: &Path, library: &Path, target: &Path) -> Option<PathBuf> {
    let target = normalize_path_lexical(&root.join(target));
    target.starts_with(library).then_some(target)
}

fn owned(root: &Path, library: &Path, name: &str) -> Result<Option<PathBuf>, SkillLinkErr> {
    let path = root.join(name);
    match fs::read_link(&path) {
        Ok(target) => Ok(owned_target(root, library, &target)),
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ) =>
        {
            Ok(None)
        }
        Err(err) => Err(io_err(&path, err)),
    }
}

pub fn plan(root: &Path, library: &Path, desired: Desired) -> Result<SkillLinkPlan, SkillLinkErr> {
    let root = absolute(root)?;
    let library = absolute(library)?;
    let mut plan = SkillLinkPlan {
        root,
        library,
        actions: Vec::new(),
        shadowed: Vec::new(),
    };
    // A library that is, or nests with, the provider root is read natively or not at
    // all; linking there would replace the library's own entries with self-links.
    if plan.root.starts_with(&plan.library) || plan.library.starts_with(&plan.root) {
        return Ok(plan);
    }
    let mut wanted = BTreeMap::new();
    if matches!(desired, Desired::Library) {
        for (name, path) in entries(&plan.library)? {
            if is_skill(&path) {
                wanted.insert(name, path);
            }
        }
    }
    for (name, _) in entries(&plan.root)? {
        let target = wanted.remove(&name);
        match owned(&plan.root, &plan.library, &name)? {
            Some(current) if target.as_ref() != Some(&current) => {
                plan.actions
                    .push(SkillLinkAction::Unlink { name: name.clone() });
                if let Some(target) = target {
                    plan.actions.push(SkillLinkAction::Link { name, target });
                }
            }
            None if target.is_some() => plan.shadowed.push(name),
            _ => {}
        }
    }
    plan.actions.extend(
        wanted
            .into_iter()
            .map(|(name, target)| SkillLinkAction::Link { name, target }),
    );
    Ok(plan)
}

pub fn apply(plan: &SkillLinkPlan) -> Result<SkillLinkOutcome, SkillLinkErr> {
    let mut outcome = SkillLinkOutcome::default();
    if plan
        .actions
        .iter()
        .any(|action| matches!(action, SkillLinkAction::Link { .. }))
    {
        fs::create_dir_all(&plan.root).map_err(|err| io_err(&plan.root, err))?;
    }
    for action in &plan.actions {
        match action {
            SkillLinkAction::Unlink { name } => {
                let path = plan.root.join(name);
                if owned(&plan.root, &plan.library, name)?.is_none() {
                    match fs::symlink_metadata(&path) {
                        Ok(_) => outcome.shadowed.push(name.clone()),
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                        Err(err) => return Err(io_err(&path, err)),
                    }
                    continue;
                }
                match fs::remove_file(&path) {
                    Ok(()) => outcome.unlinked += 1,
                    Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                    Err(err) => return Err(io_err(&path, err)),
                }
            }
            SkillLinkAction::Link { name, target } => {
                let path = plan.root.join(name);
                match std::os::unix::fs::symlink(target, &path) {
                    Ok(()) => outcome.linked += 1,
                    Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                        if owned(&plan.root, &plan.library, name)?.as_ref() != Some(target) {
                            outcome.shadowed.push(name.clone());
                        }
                    }
                    Err(err) => return Err(io_err(&path, err)),
                }
            }
        }
    }
    outcome.shadowed.sort();
    outcome.shadowed.dedup();
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_is_lexical_and_component_bounded() {
        let root = Path::new("/provider/skills");
        let library = Path::new("/rimz/skills");
        for target in ["/rimz/skills/gone", "../../rimz/skills/a/../gone"] {
            assert_eq!(
                owned_target(root, library, Path::new(target)),
                Some(library.join("gone"))
            );
        }
        for target in ["/rimz/skills-other/a", "/rimz/skills/../foreign", "local"] {
            assert_eq!(owned_target(root, library, Path::new(target)), None);
        }
    }
}
