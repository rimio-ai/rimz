//! Deployed workflows by name: the home's `workflows/<name>/`, then each installed pack's
//! `workflows/<name>/` that holds a `workflow.toml`. Names resolve through
//! [`packs::resolve_claims`], so a home directory wins by name, an excluded pack workflow resolves
//! nowhere, and two packs claiming one name conflict until one is excluded or the home claims it.
//! A pack workflow's directory is spelled through the pack's stable entry link, never its
//! commit's checkout, so a run that records it reads the pack's next update live. A name lookup
//! reads no `workflow.toml`; only [`Deployed::into_pack_items`] does, for the pack inventory.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::packs::{
    self, ClaimResolution, ItemFate, ItemType, PackClaim, PackItem, PackName, Problem,
    ResolvedPacks,
};

pub struct Deployed {
    home: PathBuf,
    resolved: BTreeMap<String, ClaimResolution>,
    items: Vec<PackItem>,
    problems: Vec<(PackName, Problem)>,
}

impl Deployed {
    /// The deployed workflows of `agents_home` and its installed packs. Fails only when the
    /// home's workflows directory exists but cannot be read.
    pub fn load(agents_home: &Path) -> std::io::Result<Self> {
        Self::from_packs(agents_home, &packs::resolve(agents_home))
    }

    pub fn from_packs(agents_home: &Path, resolved: &ResolvedPacks) -> std::io::Result<Self> {
        let home = crate::disk::paths::workflows_dir_under(agents_home);
        let mut home_claims = BTreeMap::new();
        match std::fs::read_dir(&home) {
            Ok(entries) => {
                for entry in entries {
                    let path = entry?.path();
                    if let Some(name) = path.file_name().and_then(std::ffi::OsStr::to_str)
                        && path.is_dir()
                    {
                        home_claims.insert(name.to_owned(), path);
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut shipped = Vec::new();
        let mut problems = Vec::new();
        for pack in &resolved.packs {
            let root = crate::disk::paths::workflows_dir_under(&pack.root);
            let entries = match std::fs::read_dir(&root) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => {
                    problems.push((pack.name.clone(), problem(&root, &error)));
                    continue;
                }
            };
            for entry in entries {
                let path = match entry {
                    Ok(entry) => entry.path(),
                    Err(error) => {
                        problems.push((pack.name.clone(), problem(&root, &error)));
                        break;
                    }
                };
                let Some(name) = workflow_name(&path) else {
                    continue;
                };
                if !crate::disk::paths::workflow_spec_file(&path).is_file() {
                    continue;
                }
                shipped.push((pack, name, path));
            }
        }
        shipped.sort_by(|a, b| (&a.1, &a.2).cmp(&(&b.1, &b.2)));
        let resolved = packs::resolve_claims(
            home_claims.clone(),
            shipped.iter().map(|(pack, name, path)| PackClaim {
                name: name.clone(),
                kind: ItemType::Workflows,
                pack: pack.name.clone(),
                path: path.clone(),
                excluded: pack.excluded.contains(ItemType::Workflows, name),
            }),
        );
        let items = shipped
            .into_iter()
            .map(|(pack, name, path)| {
                let fate = if pack.excluded.types.contains(&ItemType::Workflows) {
                    ItemFate::ExcludedType
                } else if pack.excluded.contains(ItemType::Workflows, &name) {
                    ItemFate::Excluded
                } else {
                    match &resolved[&name] {
                        ClaimResolution::Home(path) => ItemFate::Overridden { path: path.clone() },
                        ClaimResolution::Pack(_) => ItemFate::Loads,
                        ClaimResolution::Conflict { claims, message } => ItemFate::Conflict {
                            claims: claims.clone(),
                            message: message.clone(),
                        },
                    }
                };
                PackItem {
                    pack: pack.name.clone(),
                    kind: ItemType::Workflows,
                    name,
                    path,
                    fate,
                }
            })
            .collect();
        Ok(Self {
            home,
            resolved,
            items,
            problems,
        })
    }

    /// The home's workflows directory, where a deployed name is looked up first.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The directory workflow `name` leads to: the home's, else the one pack's that claims it;
    /// `None` when nothing claims it, and the conflict's message when two packs do.
    pub fn find(&self, name: &str) -> Result<Option<&Path>, &str> {
        match self.resolved.get(name) {
            None => Ok(None),
            Some(ClaimResolution::Home(path)) => Ok(Some(path)),
            Some(ClaimResolution::Pack(claim)) => Ok(Some(&claim.path)),
            Some(ClaimResolution::Conflict { message, .. }) => Err(message),
        }
    }

    /// Every deployed workflow by name, with its directory or its conflict's message; a home
    /// dot entry resolves only when named.
    pub fn every(&self) -> impl Iterator<Item = (&str, Result<&Path, &str>)> {
        self.resolved
            .keys()
            .filter(|name| !name.starts_with('.'))
            .map(|name| {
                (
                    name.as_str(),
                    self.find(name).map(|dir| dir.expect("a claimed name")),
                )
            })
    }

    /// `dir` as its claim spells it, links followed: the directory a deployed name leads to, or
    /// one nested in the home's workflows directory, under the home; `None` for any other.
    pub fn claim(&self, dir: &Path) -> Option<PathBuf> {
        let dir = dir.canonicalize().ok()?;
        let named = self.every().find_map(|(_, found)| {
            let found = found.ok()?;
            (found.canonicalize().ok()? == dir).then(|| found.to_owned())
        });
        named.or_else(|| {
            let nested = dir.strip_prefix(self.home.canonicalize().ok()?).ok()?;
            (!nested.as_os_str().is_empty()).then(|| self.home.join(nested))
        })
    }

    /// Each pack's workflow items with their fates, and each pack whose `workflows/` could not
    /// be read. An item that holds its name is read through the loader and is `Failed` when
    /// refused; an excluded, overridden, or conflicting one is not read.
    pub(crate) fn into_pack_items(mut self) -> (Vec<PackItem>, Vec<(PackName, Problem)>) {
        for item in &mut self.items {
            if item.fate != ItemFate::Loads {
                continue;
            }
            if let Err(error) = super::spec::load(&item.path) {
                item.fate = ItemFate::Failed {
                    message: error.one_line(),
                };
            }
        }
        (self.items, self.problems)
    }
}

/// The workflow name a directory entry carries: none for a dot entry or a name that is not
/// UTF-8.
fn workflow_name(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    (!name.starts_with('.')).then(|| name.to_owned())
}

fn problem(path: &Path, error: &std::io::Error) -> Problem {
    Problem {
        path: path.to_owned(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "[workflow]\nstart = 'a'\n";
    const LOADABLE: &str = "[workflow]\nscope = 'worktree'\nstart = 'a'\n\
                            [script.a]\ntimeout = '1m'\nrun = 'true'\non.ok = '@done'\n";

    fn write(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("workflow.toml"), SPEC).unwrap();
    }

    #[test]
    fn home_wins_packs_claim_by_name_and_exclusions_and_conflicts_hold() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let source = root.path().join("source");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(
            source.join("rimz-packs.toml"),
            "[[pack]]\nname = 'crew'\ndescription = 'Crew'\npath = 'crew'\n\
             [[pack]]\nname = 'tools'\ndescription = 'Tools'\npath = 'tools'\n",
        )
        .unwrap();
        for dir in [
            "crew/workflows/ship",
            "crew/workflows/both",
            "crew/workflows/mine",
            "crew/workflows/.draft",
            "tools/workflows/both",
        ] {
            write(&source.join(dir));
        }
        std::fs::create_dir_all(source.join("crew/workflows/notes")).unwrap();
        write(&home.join("workflows/mine"));
        write(&home.join("workflows/own"));
        write(&home.join("workflows/.draft"));
        write(&home.join("workflows/group/nested"));
        crate::packs::install(&home, &source).unwrap();
        let entry = home.join("packs/source");
        let deployed = Deployed::load(&home).unwrap();
        let conflict = deployed.find("both").unwrap_err().to_owned();
        assert!(
            conflict.starts_with("pack conflict for `both`: crew (")
                && conflict.contains("), tools ("),
            "{conflict}"
        );
        let every: Vec<_> = deployed
            .every()
            .map(|(name, found)| {
                (
                    name.to_owned(),
                    found.map(Path::to_owned).map_err(str::to_owned),
                )
            })
            .collect();
        assert_eq!(
            every,
            [
                ("both".to_owned(), Err(conflict.clone())),
                ("group".into(), Ok(home.join("workflows/group"))),
                ("mine".into(), Ok(home.join("workflows/mine"))),
                ("own".into(), Ok(home.join("workflows/own"))),
                ("ship".into(), Ok(entry.join("crew/workflows/ship"))),
            ]
        );
        assert_eq!(deployed.find("notes"), Ok(None));
        // A home dot entry resolves by name, and a pack's stays unclaimed.
        assert_eq!(
            deployed.find(".draft"),
            Ok(Some(home.join("workflows/.draft").as_path()))
        );
        let ship = entry.join("crew/workflows/ship");
        assert_eq!(deployed.claim(&ship), Some(ship.clone()));
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(home.join("workflows/own"), &alias).unwrap();
        assert_eq!(deployed.claim(&alias), Some(home.join("workflows/own")));
        assert_eq!(
            deployed.claim(&home.join("workflows/group/nested")),
            Some(home.join("workflows/group/nested"))
        );
        assert_eq!(deployed.claim(&home.join("workflows")), None);
        assert_eq!(deployed.claim(&entry.join("crew/workflows/mine")), None);
        assert_eq!(deployed.claim(&source.join("tools/workflows/both")), None);
        let conflicting = Deployed::load(&home).unwrap().into_pack_items().0;
        assert!(
            conflicting
                .iter()
                .filter(|item| item.name == "both")
                .all(|item| matches!(item.fate, ItemFate::Conflict { .. })),
            "{conflicting:?}"
        );

        crate::packs::exclude(
            &home,
            &"tools".parse().unwrap(),
            &crate::packs::ExclusionTarget::Type(ItemType::Workflows),
        )
        .unwrap();
        crate::packs::exclude(
            &home,
            &"crew".parse().unwrap(),
            &crate::packs::ExclusionTarget::Item(crate::packs::ItemKey {
                kind: ItemType::Workflows,
                name: "ship".into(),
            }),
        )
        .unwrap();
        let deployed = Deployed::load(&home).unwrap();
        assert_eq!(
            deployed.find("both"),
            Ok(Some(entry.join("crew/workflows/both").as_path()))
        );
        assert_eq!(deployed.find("ship"), Ok(None));
        std::fs::write(source.join("crew/workflows/both/workflow.toml"), LOADABLE).unwrap();
        for (name, text) in [
            ("syntax", b"this is not toml\n".as_slice()),
            ("unknown", b"[workflow]\n\"bad\\nkey\" = 1\n"),
            ("binary", b"\xff\xfe"),
        ] {
            let dir = source.join("crew/workflows").join(name);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("workflow.toml"), text).unwrap();
        }
        let deployed = Deployed::load(&home).unwrap();
        assert!(deployed.find("syntax").unwrap().is_some());
        let (failed, fates): (Vec<_>, Vec<_>) = deployed
            .into_pack_items()
            .0
            .into_iter()
            .map(|item| (item.pack.as_str().to_owned(), item.name, item.fate))
            .partition(|(_, _, fate)| matches!(fate, ItemFate::Failed { .. }));
        let failed: Vec<_> = failed
            .iter()
            .map(|(_, name, fate)| (name.as_str(), fate.problem().unwrap()))
            .collect();
        assert_eq!(
            failed.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            ["binary", "syntax", "unknown"],
            "{failed:?}"
        );
        for (_, message) in &failed {
            assert_eq!(message.lines().count(), 1, "{message:?}");
            assert!(!message.contains("this is not toml"), "{message:?}");
        }
        assert_eq!(
            fates,
            [
                ("crew".to_owned(), "both".to_owned(), ItemFate::Loads),
                ("tools".into(), "both".into(), ItemFate::ExcludedType),
                (
                    "crew".into(),
                    "mine".into(),
                    ItemFate::Overridden {
                        path: home.join("workflows/mine")
                    }
                ),
                ("crew".into(), "ship".into(), ItemFate::Excluded),
            ]
        );
    }
}
