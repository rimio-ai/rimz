//! Home overrides of shipped definitions and skill trees.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::disk::atomic;
use crate::packs::{ItemFate, ItemType, PackItem};
use crate::utils::path::normalize_path_lexical;

use super::frontmatter::AgentFrontmatter;
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
    #[error(
        "`--set` needs an inheriting override, and this item supports only a copy; \
         run `{command}` without `--set`"
    )]
    SetOnCopy { command: String },
    #[error("two targets write {path}; customize each item once")]
    SameDestination { path: PathBuf },
    #[error("--set {field}={value}: {reason}")]
    Field {
        field: String,
        value: String,
        reason: String,
    },
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

    fn stage(
        &self,
        copy: bool,
        fields: &[(String, String)],
        destination: PathBuf,
    ) -> Result<Staged, OverrideErr> {
        let parent = self.home.join(self.item.kind.as_str());
        atomic::create_dir_all_mode(&parent, 0o755)?;
        let stage = Stage(parent.join(format!(".customize-{}", uuid::Uuid::now_v7())));
        atomic::create_dir_all_mode(&stage.0, 0o700)?;
        let staged = stage.0.join("item");
        if is_tree(self.item.kind) {
            copy_tree(&self.item.path, &staged)?;
        } else {
            let bytes = if copy {
                read(&self.item.path)?
            } else {
                self.inheriting_stub(fields)?.into_bytes()
            };
            atomic::write_bytes_if_absent_mode(&staged, &bytes, 0o644)?;
        }
        atomic::sync_tree(&stage.0)?;
        Ok(Staged {
            _stage: stage,
            staged,
            destination,
        })
    }

    /// Refuses what `stage` would refuse, writing nothing, and returns the destination.
    fn check(&self, copy: bool, fields: &[(String, String)]) -> Result<PathBuf, OverrideErr> {
        self.require_included()?;
        if let ItemFate::Overridden { path } = &self.item.fate {
            return Err(OverrideErr::Overridden {
                path: path.clone(),
                diff: self.command("diff"),
                reset: self.command("reset"),
            });
        }
        if !fields.is_empty() && (copy || self.copy_only()) {
            return Err(OverrideErr::SetOnCopy {
                command: format!("{} --copy", self.command("customize")),
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
        let destination = self.home.join(self.item.kind.as_str()).join(filename);
        require_absent(&destination)?;
        if !copy {
            self.inheriting_stub(fields)?;
        }
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
        if !is_tree(self.item.kind) {
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
        let custom_root = override_entry(self.item.kind, path);
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
        if !is_tree(self.item.kind) {
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

    fn inheriting_stub(&self, fields: &[(String, String)]) -> Result<String, OverrideErr> {
        let text =
            fs::read_to_string(&self.item.path).map_err(|error| io(&self.item.path, error))?;
        let (yaml, _) = frontmatter::split(&self.item.path, &text)?;
        #[derive(serde::Deserialize)]
        struct Description {
            description: Option<String>,
        }
        let description: Description = frontmatter::parse(&self.item.path, yaml)?;
        frontmatter::description(&self.item.path, description.description.as_deref())?;
        let mut entries = vec![
            ("agent".to_owned(), serde_json::json!(self.key())),
            (
                "description".to_owned(),
                serde_json::json!(description.description),
            ),
        ];
        if self
            .item
            .path
            .file_stem()
            .is_none_or(|stem| stem != self.item.name.as_str())
        {
            entries.push(("name".to_owned(), serde_json::json!(self.item.name)));
        }
        for (field, value) in fields {
            let refuse = |reason: String| OverrideErr::Field {
                field: field.clone(),
                value: value.clone(),
                reason,
            };
            let mut chars = field.chars();
            let plain = chars.next().is_some_and(|first| first.is_ascii_lowercase())
                && chars.all(|c| {
                    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_')
                });
            if !plain {
                return Err(refuse(
                    "is not a field name; use lowercase letters, digits, `-`, and `_`".to_owned(),
                ));
            }
            if matches!(field.as_str(), "agent" | "name") {
                return Err(refuse("customize writes this field itself".to_owned()));
            }
            let parsed: serde_json::Value = serde_saphyr::from_str(value)
                .map_err(|error| refuse(format!("is not one YAML value: {error}")))?;
            match entries.iter_mut().find(|(key, _)| key == field) {
                Some(entry) => entry.1 = parsed,
                None => entries.push((field.clone(), parsed)),
            }
            frontmatter::parse::<AgentFrontmatter>(&self.item.path, &yaml_lines(&entries))
                .map_err(|error| refuse(error.to_string()))?
                .check_plus()
                .map_err(refuse)?;
        }
        Ok(format!("---\n{}---\n", yaml_lines(&entries)))
    }
}

/// Writes an override of each item and returns their paths in order. Every item is checked
/// first, so a refusal writes nothing. `fields` are `--set` pairs written into each
/// inheriting override; a copy refuses them.
pub fn customize(
    home: &Path,
    items: &[&PackItem],
    copy: bool,
    fields: &[(String, String)],
) -> Result<Vec<PathBuf>, OverrideErr> {
    let mut destinations = Vec::new();
    for &item in items {
        let path = PackOverride::new(home, item).check(copy, fields)?;
        if destinations.contains(&path) {
            return Err(OverrideErr::SameDestination { path });
        }
        destinations.push(path);
    }
    let staged = items
        .iter()
        .zip(destinations)
        .map(|(&item, destination)| PackOverride::new(home, item).stage(copy, fields, destination))
        .collect::<Result<Vec<_>, _>>()?;
    staged.into_iter().map(Staged::publish).collect()
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

/// One `key: value` line per entry. JSON is YAML flow syntax, so strings keep their punctuation and
/// whitespace verbatim and a list stays one value.
fn yaml_lines(entries: &[(String, serde_json::Value)]) -> String {
    entries
        .iter()
        .map(|(key, value)| format!("{key}: {value}\n"))
        .collect()
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

/// Whether an item of `kind` is a directory, copied, compared, and reset as a whole tree.
fn is_tree(kind: ItemType) -> bool {
    matches!(kind, ItemType::Skills | ItemType::Workflows)
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

/// An override written to its stage, awaiting a no-replace publish. Dropping it removes the stage.
struct Staged {
    _stage: Stage,
    staged: PathBuf,
    destination: PathBuf,
}

impl Staged {
    fn publish(self) -> Result<PathBuf, OverrideErr> {
        atomic::publish_staged_no_replace(&self.staged, &self.destination).map_err(|error| {
            match error {
                atomic::AtomicErr::Exists { path } => OverrideErr::Exists { path },
                error => error.into(),
            }
        })?;
        Ok(self.destination)
    }
}

struct Stage(PathBuf);

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
