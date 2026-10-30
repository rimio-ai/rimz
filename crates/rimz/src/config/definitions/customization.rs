//! Home overrides of shipped definitions and skill trees.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::disk::atomic;
use crate::packs::{ItemFate, ItemType, PackItem};
use crate::utils::path::normalize_path_lexical;

use super::{DefinitionErr, catalog, frontmatter};

#[derive(Debug, thiserror::Error)]
pub enum OverrideErr {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Atomic(#[from] atomic::AtomicErr),
    #[error(transparent)]
    Definition(#[from] DefinitionErr),
    #[error("item is excluded; run `{command}` first")]
    Excluded { command: String },
    #[error(
        "already overridden by {path}; inspect with `{diff}`; \
         `{reset}` removes only overrides in this item's own home tree"
    )]
    Overridden {
        path: PathBuf,
        diff: String,
        reset: String,
    },
    #[error("destination already exists: {path}; nothing replaced")]
    Exists { path: PathBuf },
    #[error("this item supports only a copy; run `{command}`")]
    CopyOnly { command: String },
    #[error("cannot inherit failed item {path}; preserve its bytes with `{command}`")]
    Failed { path: PathBuf, command: String },
    #[error("refusing to remove override outside this item's home tree: {path}")]
    OutsideTree { path: PathBuf },
    #[error("refusing to remove configured command override: {path}")]
    ConfiguredCommand { path: PathBuf },
    #[error(
        "{path}: home entry resolves to the shipped item {shipped} or contains it; \
         nothing was removed from this entry"
    )]
    ShippedItem { path: PathBuf, shipped: PathBuf },
    #[error("refusing symlink or non-regular entry: {path}")]
    NotRegular { path: PathBuf },
}

pub struct PackOverride<'a> {
    home: &'a Path,
    item: &'a PackItem,
}

pub enum OverrideDiff {
    None,
    Inherits {
        text: String,
    },
    Command {
        path: PathBuf,
    },
    Files {
        path: PathBuf,
        changed: Vec<FileDiff>,
    },
}

pub struct FileDiff {
    pub shipped_path: PathBuf,
    pub path: PathBuf,
    pub shipped: Option<Vec<u8>>,
    pub custom: Option<Vec<u8>>,
}

pub enum ResetOutcome<'a> {
    Removed {
        item: &'a PackItem,
        path: PathBuf,
    },
    Kept {
        item: &'a PackItem,
        reason: Option<String>,
    },
    Failed {
        item: &'a PackItem,
        error: String,
    },
}

impl<'a> PackOverride<'a> {
    pub fn new(home: &'a Path, item: &'a PackItem) -> Self {
        Self { home, item }
    }

    pub fn customize(&self, copy: bool) -> Result<PathBuf, OverrideErr> {
        self.require_included()?;
        if let ItemFate::Overridden { path } = &self.item.fate {
            return Err(OverrideErr::Overridden {
                path: path.clone(),
                diff: self.command("diff"),
                reset: self.command("reset"),
            });
        }
        if !copy && self.copy_only() {
            return Err(OverrideErr::CopyOnly {
                command: format!("{} --copy", self.command("customize")),
            });
        }
        if !copy && matches!(self.item.fate, ItemFate::Failed { .. }) {
            return Err(OverrideErr::Failed {
                path: self.item.path.clone(),
                command: format!("{} --copy", self.command("customize")),
            });
        }
        let filename = self
            .item
            .path
            .file_name()
            .ok_or_else(|| OverrideErr::NotRegular {
                path: self.item.path.clone(),
            })?;
        let parent = self.home.join(self.item.kind.as_str());
        let destination = parent.join(filename);
        require_absent(&destination)?;
        atomic::create_dir_all_mode(&parent, 0o755)?;
        let stage = Stage(parent.join(format!(".customize-{}", uuid::Uuid::now_v7())));
        atomic::create_dir_all_mode(&stage.0, 0o700)?;
        let staged = stage.0.join("item");
        if self.item.kind == ItemType::Skills {
            copy_tree(&self.item.path, &staged)?;
        } else {
            let bytes = if copy {
                read(&self.item.path)?
            } else {
                self.inheriting_stub()?.into_bytes()
            };
            atomic::write_bytes_if_absent_mode(&staged, &bytes, 0o644)?;
        }
        atomic::sync_tree(&stage.0)?;
        atomic::publish_staged_no_replace(&staged, &destination).map_err(|error| match error {
            atomic::AtomicErr::Exists { path } => OverrideErr::Exists { path },
            error => error.into(),
        })?;
        Ok(destination)
    }

    pub fn diff(&self) -> Result<OverrideDiff, OverrideErr> {
        self.require_included()?;
        let ItemFate::Overridden { path } = &self.item.fate else {
            return Ok(OverrideDiff::None);
        };
        if self.is_command_override(path) {
            return Ok(OverrideDiff::Command { path: path.clone() });
        }
        if self.item.kind != ItemType::Skills {
            let custom = read(path)?;
            if let Ok(text) = std::str::from_utf8(&custom)
                && let Ok((yaml, _)) = frontmatter::split(path, text)
            {
                #[derive(serde::Deserialize)]
                struct Parent {
                    agent: Option<String>,
                }
                if serde_saphyr::from_str::<Parent>(yaml)
                    .ok()
                    .and_then(|parent| parent.agent)
                    .as_deref()
                    == Some(&self.key())
                {
                    return Ok(OverrideDiff::Inherits {
                        text: text.to_owned(),
                    });
                }
            }
            let shipped = read(&self.item.path)?;
            let changed = (shipped != custom).then(|| FileDiff {
                shipped_path: self.item.path.clone(),
                path: path.clone(),
                shipped: Some(shipped),
                custom: Some(custom),
            });
            return Ok(OverrideDiff::Files {
                path: path.clone(),
                changed: changed.into_iter().collect(),
            });
        }
        // SkillChain records every skill override at <entry>/SKILL.md.
        let custom_root = path.parent().expect("skill override points to SKILL.md");
        let mut shipped = tree_files(&self.item.path, true)?;
        let mut custom = tree_files(custom_root, true)?;
        let paths: BTreeSet<_> = shipped.keys().chain(custom.keys()).cloned().collect();
        Ok(OverrideDiff::Files {
            path: custom_root.to_path_buf(),
            changed: paths
                .into_iter()
                .map(|path| FileDiff {
                    shipped: shipped.remove(&path),
                    custom: custom.remove(&path),
                    shipped_path: self.item.path.join(&path),
                    path: custom_root.join(path),
                })
                .filter(|file| file.shipped != file.custom)
                .collect(),
        })
    }

    pub fn identical(&self) -> Result<bool, OverrideErr> {
        self.require_included()?;
        let Some(path) = self.own_path()? else {
            return Ok(false);
        };
        self.identical_at(&path)
    }

    fn identical_at(&self, path: &Path) -> Result<bool, OverrideErr> {
        if self.item.kind != ItemType::Skills {
            return Ok(read(&self.item.path)? == read(path)?);
        }
        let compare = || -> Result<bool, OverrideErr> {
            Ok(tree_files(&self.item.path, false)? == tree_files(path, false)?)
        };
        match compare() {
            Err(OverrideErr::NotRegular { .. }) => Ok(false),
            result => result,
        }
    }

    pub fn reset(&self, identical: bool) -> Result<Option<PathBuf>, OverrideErr> {
        self.require_included()?;
        let Some(path) = self.own_path()? else {
            return Ok(None);
        };
        let metadata = fs::symlink_metadata(&path).map_err(|error| io(&path, error))?;
        if identical && !self.identical()? {
            return Ok(None);
        }
        if metadata.is_dir() {
            fs::remove_dir_all(&path).map_err(|error| io(&path, error))?;
        } else {
            fs::remove_file(&path).map_err(|error| io(&path, error))?;
        }
        // own_path admitted a direct child of the item's home tree.
        atomic::sync_dir(path.parent().expect("home entry has a parent"))?;
        Ok(Some(path))
    }

    fn own_path(&self) -> Result<Option<PathBuf>, OverrideErr> {
        let ItemFate::Overridden { path } = &self.item.fate else {
            return Ok(None);
        };
        if self.is_command_override(path) {
            return Err(OverrideErr::ConfiguredCommand { path: path.clone() });
        }
        let entry = override_entry(self.item.kind, path);
        let tree = self.home.join(self.item.kind.as_str());
        let absolute_entry = std::path::absolute(entry).map_err(|error| io(entry, error))?;
        let absolute_tree = std::path::absolute(&tree).map_err(|error| io(&tree, error))?;
        let absolute_entry = normalize_path_lexical(&absolute_entry);
        let absolute_tree = normalize_path_lexical(&absolute_tree);
        if absolute_entry.parent() != Some(absolute_tree.as_path()) {
            return Err(OverrideErr::OutsideTree { path: path.clone() });
        }
        if resolved_entry(&self.item.path)?.starts_with(resolved_entry(entry)?) {
            return Err(OverrideErr::ShippedItem {
                path: entry.to_owned(),
                shipped: self.item.path.clone(),
            });
        }
        Ok(Some(entry.to_owned()))
    }

    fn is_command_override(&self, path: &Path) -> bool {
        self.item.kind == ItemType::Teams && path.extension().is_some_and(|ext| ext == "toml")
    }

    fn require_included(&self) -> Result<(), OverrideErr> {
        let command = match self.item.fate {
            ItemFate::ExcludedType => command([
                "rimz",
                "packs",
                "include",
                self.item.pack.as_str(),
                "--type",
                self.item.kind.as_str(),
            ]),
            ItemFate::Excluded => self.command("include"),
            _ => return Ok(()),
        };
        Err(OverrideErr::Excluded { command })
    }

    fn copy_only(&self) -> bool {
        !matches!(self.item.kind, ItemType::Agents | ItemType::Subagents)
            || catalog::kind_base(self.item.kind, &self.item.path, &self.item.name)
    }

    fn key(&self) -> String {
        format!("{}:{}", self.item.pack, self.item.name)
    }

    fn command(&self, verb: &str) -> String {
        command([
            "rimz",
            "packs",
            verb,
            &self.key(),
            "--type",
            self.item.kind.as_str(),
        ])
    }

    fn inheriting_stub(&self) -> Result<String, OverrideErr> {
        let text =
            fs::read_to_string(&self.item.path).map_err(|error| io(&self.item.path, error))?;
        let (yaml, _) = frontmatter::split(&self.item.path, &text)?;
        #[derive(serde::Deserialize)]
        struct Description {
            description: Option<String>,
        }
        let description: Description = frontmatter::parse(&self.item.path, yaml)?;
        frontmatter::description(&self.item.path, description.description.as_deref())?;
        // JSON-quoted strings are YAML scalars too; preserve punctuation and whitespace verbatim.
        let mut stub = format!(
            "---\nagent: {}\ndescription: {}\n",
            serde_json::json!(self.key()),
            serde_json::json!(description.description)
        );
        if self
            .item
            .path
            .file_stem()
            .is_none_or(|stem| stem != self.item.name.as_str())
        {
            stub.push_str(&format!("name: {}\n", serde_json::json!(self.item.name)));
        }
        stub.push_str("---\n");
        Ok(stub)
    }
}

pub fn reset_identical<'a>(
    home: &Path,
    items: impl IntoIterator<Item = &'a PackItem>,
) -> Vec<ResetOutcome<'a>> {
    let mut entries = BTreeMap::<PathBuf, Vec<&PackItem>>::new();
    let mut outcomes = Vec::new();
    for item in items {
        let ItemFate::Overridden { path } = &item.fate else {
            continue;
        };
        match resolved_entry(override_entry(item.kind, path)) {
            Ok(entry) => entries.entry(entry).or_default().push(item),
            Err(reason) => outcomes.push(ResetOutcome::Kept {
                item,
                reason: Some(reason.to_string()),
            }),
        }
    }
    for items in entries.into_values() {
        let outcome = reset_identical_entry(home, &items);
        for item in items {
            outcomes.push(match &outcome {
                ResetOutcome::Removed { path, .. } => ResetOutcome::Removed {
                    item,
                    path: path.clone(),
                },
                ResetOutcome::Kept { reason, .. } => ResetOutcome::Kept {
                    item,
                    reason: PackOverride::new(home, item)
                        .own_path()
                        .err()
                        .map(|error| error.to_string())
                        .or_else(|| reason.clone()),
                },
                ResetOutcome::Failed { error, .. } => ResetOutcome::Failed {
                    item,
                    error: error.clone(),
                },
            });
        }
    }
    outcomes
}

fn reset_identical_entry<'a>(home: &Path, items: &[&'a PackItem]) -> ResetOutcome<'a> {
    let mut eligible = None;
    let mut refusal = None;
    for &item in items {
        match PackOverride::new(home, item).own_path() {
            Ok(Some(path)) => {
                eligible.get_or_insert((item, path));
            }
            Err(reason @ OverrideErr::OutsideTree { .. }) => {
                refusal.get_or_insert((item, reason));
            }
            Err(reason) => {
                return ResetOutcome::Kept {
                    item,
                    reason: Some(reason.to_string()),
                };
            }
            Ok(None) => {}
        }
    }
    let Some((item, path)) = eligible else {
        // Each group contains immutable Overridden inventory items.
        let (item, reason) = refusal.expect("an override group has an entry or a refusal");
        return ResetOutcome::Kept {
            item,
            reason: Some(reason.to_string()),
        };
    };
    let entry = PackOverride::new(home, item);
    match entry.identical_at(&path) {
        Ok(true) => {}
        Ok(false) => return ResetOutcome::Kept { item, reason: None },
        Err(reason) => {
            return ResetOutcome::Kept {
                item,
                reason: Some(reason.to_string()),
            };
        }
    }
    match entry.reset(false) {
        Ok(path) => ResetOutcome::Removed {
            item,
            // The selected item's immutable fate is still Overridden.
            path: path.expect("a selected override has a home entry"),
        },
        Err(error) => ResetOutcome::Failed {
            item,
            error: error.to_string(),
        },
    }
}

fn override_entry(kind: ItemType, path: &Path) -> &Path {
    if kind == ItemType::Skills {
        // SkillChain records every skill override at <entry>/SKILL.md.
        path.parent().expect("skill override points to SKILL.md")
    } else {
        path
    }
}

fn command<const N: usize>(args: [&str; N]) -> String {
    shlex::try_join(args)
        .unwrap_or_else(|_| "item name contains NUL and cannot be passed as an argument".to_owned())
}

fn io(path: &Path, source: std::io::Error) -> OverrideErr {
    OverrideErr::Io {
        path: path.to_owned(),
        source,
    }
}

fn read(path: &Path) -> Result<Vec<u8>, OverrideErr> {
    fs::read(path).map_err(|error| io(path, error))
}

fn resolved_entry(path: &Path) -> Result<PathBuf, OverrideErr> {
    // Inventory entries always name a file or skill directory below a tree.
    let parent = path.parent().expect("inventory entry has a parent");
    let name = path.file_name().expect("inventory entry has a file name");
    Ok(fs::canonicalize(parent)
        .map_err(|error| io(path, error))?
        .join(name))
}

fn require_absent(path: &Path) -> Result<(), OverrideErr> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(OverrideErr::Exists {
            path: path.to_owned(),
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(io(path, error)),
    }
}

fn copy_tree(source: &Path, destination: &Path) -> Result<(), OverrideErr> {
    let metadata = fs::symlink_metadata(source).map_err(|error| io(source, error))?;
    if !metadata.is_dir() {
        return Err(OverrideErr::NotRegular {
            path: source.to_owned(),
        });
    }
    atomic::create_dir_all_mode(destination, 0o755)?;
    for entry in fs::read_dir(source).map_err(|error| io(source, error))? {
        let entry = entry.map_err(|error| io(source, error))?;
        let path = entry.path();
        let kind = entry.file_type().map_err(|error| io(&path, error))?;
        let target = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&path, &target)?;
        } else if kind.is_file() {
            fs::copy(&path, &target).map_err(|error| io(&path, error))?;
        } else {
            return Err(OverrideErr::NotRegular { path });
        }
    }
    Ok(())
}

fn tree_files(root: &Path, show_symlinks: bool) -> Result<BTreeMap<PathBuf, Vec<u8>>, OverrideErr> {
    fn collect(
        root: &Path,
        relative: &Path,
        show_symlinks: bool,
        files: &mut BTreeMap<PathBuf, Vec<u8>>,
    ) -> Result<(), OverrideErr> {
        let directory = root.join(relative);
        for entry in fs::read_dir(&directory).map_err(|error| io(&directory, error))? {
            let entry = entry.map_err(|error| io(&directory, error))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|error| io(&path, error))?;
            let relative = relative.join(entry.file_name());
            if kind.is_dir() {
                collect(root, &relative, show_symlinks, files)?;
            } else if kind.is_file() {
                files.insert(relative, read(&path)?);
            } else if kind.is_symlink() && show_symlinks {
                let target = fs::read_link(&path).map_err(|error| io(&path, error))?;
                files.insert(
                    relative,
                    format!("symlink to {}\n", target.display()).into_bytes(),
                );
            } else {
                return Err(OverrideErr::NotRegular { path });
            }
        }
        Ok(())
    }
    let mut files = BTreeMap::new();
    collect(root, Path::new(""), show_symlinks, &mut files)?;
    Ok(files)
}

struct Stage(PathBuf);

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
