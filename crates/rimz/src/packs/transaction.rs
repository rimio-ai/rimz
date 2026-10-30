//! Held Git checkouts and optimistic publication shared by pack mutations.

use super::*;
use std::collections::VecDeque;

struct Staged {
    checkout: PathBuf,
    manifest: Manifest,
    /// The installed checkout a requirement joined, not a tree this transaction fetched.
    held: bool,
    _hold: WorkspaceLock,
}

pub struct Transaction {
    packs_dir: PathBuf,
    before: Record,
    after: Record,
    staged: BTreeMap<String, Staged>,
    required_by: BTreeMap<PackName, PackName>,
}

/// Retains replaced checkouts until the owning command drops the guard.
#[derive(Debug)]
pub struct RetiredCheckouts {
    _holds: Vec<WorkspaceLock>,
}

pub struct PackChange<'a> {
    pub name: &'a PackName,
    pub source: &'a InstalledSource,
    pub previous: Option<&'a InstalledSource>,
    pub required_by: Option<&'a PackName>,
}

impl Transaction {
    /// Resolve refs and stage durable checkouts without taking the installation-record lock.
    pub fn stage(home: &Path, source: &GitSource) -> Result<Self> {
        Self::stage_with(home, source, &git::Client::default())
    }

    /// Stage a following Git source's installed selection, refusing a stale source snapshot.
    pub fn stage_update(home: &Path, installed: &InstalledSource) -> Result<Self> {
        let source = GitSource {
            source: installed.source.clone(),
            url: installed
                .url
                .clone()
                .unwrap_or_else(|| installed.source.clone()),
            pack: None,
            reference: None,
        };
        Self::stage_selected(home, &source, &git::Client::default(), Some(installed))
    }

    pub(super) fn stage_with(
        home: &Path,
        source: &GitSource,
        client: &git::Client,
    ) -> Result<Self> {
        Self::stage_selected(home, source, client, None)
    }

    fn stage_selected(
        home: &Path,
        source: &GitSource,
        client: &git::Client,
        installed: Option<&InstalledSource>,
    ) -> Result<Self> {
        let root = paths::packs_dir(home);
        let root = std::path::absolute(&root).map_err(|error| io_err(&root, error))?;
        let root = normalize_path_lexical(&root);
        let before = read_record(&root)?;
        if installed.is_some_and(|source| !before.sources.contains(source)) {
            return Err(PackErr::RecordChanged);
        }
        let new_parents: Vec<_> = root
            .ancestors()
            .take_while(|path| !path.exists())
            .filter_map(Path::parent)
            .map(Path::to_owned)
            .collect();
        fs::create_dir_all(&root).map_err(|error| io_err(&root, error))?;
        for parent in new_parents {
            atomic::sync_dir(&parent)?;
        }
        let mut transaction = Self {
            packs_dir: root,
            after: before.clone(),
            before,
            staged: BTreeMap::new(),
            required_by: BTreeMap::new(),
        };
        let index = transaction.fetch(source, client)?;
        let manifest = &transaction.staged[&source.source].manifest;
        let selected = if let Some(installed) = installed {
            installed
                .packs
                .iter()
                .map(|pack| pack.name.clone())
                .collect()
        } else {
            let defaults = manifest.packs.iter().any(|pack| pack.default);
            source.pack.as_ref().map_or_else(
                || {
                    manifest
                        .packs
                        .iter()
                        .filter(|pack| !defaults || pack.default)
                        .map(|pack| pack.name.clone())
                        .collect::<Vec<_>>()
                },
                |name| vec![name.clone()],
            )
        };
        for name in selected {
            transaction.select(index, &name, None)?;
        }
        transaction.requirements(index, client)?;
        Ok(transaction)
    }

    fn fetch(&mut self, source: &GitSource, client: &git::Client) -> Result<usize> {
        let revision = client.resolve(&self.packs_dir, source)?;
        let existing = self
            .after
            .sources
            .iter()
            .position(|old| old.source == source.source);
        let next = InstalledSource {
            entry: existing
                .map(|index| self.after.sources[index].entry.clone())
                .unwrap_or_else(|| entry_name(Path::new(&source.source), &self.after)),
            source: source.source.clone(),
            url: Some(source.url.clone()),
            linked: false,
            reference: Some(revision.reference.clone()),
            pinned: source.reference.is_some(),
            commit: Some(revision.commit.clone()),
            packs: existing
                .map(|index| self.after.sources[index].packs.clone())
                .unwrap_or_default(),
        };
        let root = layout::checkout(&self.packs_dir, &next)?;
        let hold = WorkspaceLock::acquire(&root.with_extension("lock"))?;
        let ready = root.join(".git/rimz-ready");
        if !ready.is_file() {
            client.checkout(&root, source, &revision)?;
            atomic::sync_tree(&root)?;
            atomic::write_bytes_atomically(&ready, b"")?;
        }
        let manifest = Manifest::read(&root)?;
        self.staged.insert(
            source.source.clone(),
            Staged {
                checkout: root,
                manifest,
                held: false,
                _hold: hold,
            },
        );
        if let Some(index) = existing {
            self.after.sources[index] = next;
            let names: Vec<_> = self.after.sources[index]
                .packs
                .iter()
                .map(|pack| pack.name.clone())
                .collect();
            for name in names {
                self.select(index, &name, None)?;
            }
            return Ok(index);
        }
        self.after.sources.push(next);
        Ok(self.after.sources.len() - 1)
    }

    fn hold_installed(&mut self, index: usize) -> Result<()> {
        let source = &self.after.sources[index];
        let root = layout::checkout(&self.packs_dir, source)?;
        let hold = WorkspaceLock::acquire(&root.with_extension("lock"))?;
        let manifest = Manifest::read(&root)?;
        self.staged.insert(
            source.source.clone(),
            Staged {
                checkout: root,
                manifest,
                held: true,
                _hold: hold,
            },
        );
        Ok(())
    }

    fn select(
        &mut self,
        index: usize,
        name: &PackName,
        required_by: Option<&PackName>,
    ) -> Result<()> {
        let source = &self.after.sources[index];
        let staged = &self.staged[&source.source];
        let manifest = &staged.manifest;
        if !manifest.packs.iter().any(|pack| pack.name == *name) {
            let installed = self
                .before
                .sources
                .iter()
                .any(|old| old.source == source.source && old.commit == source.commit);
            let at = if installed {
                " at its installed commit"
            } else {
                ""
            };
            let available = manifest
                .packs
                .iter()
                .map(|pack| pack.name.to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let spelling = GitSource {
                source: source.source.clone(),
                url: source.url.clone().unwrap_or_else(|| source.source.clone()),
                pack: None,
                reference: None,
            };
            let fix = if source.packs.iter().any(|pack| pack.name == *name) {
                format!("; remove it with `rimz packs remove {name}` before retrying")
            } else if staged.held {
                let selected = GitSource {
                    pack: Some(name.clone()),
                    ..spelling.clone()
                }
                .spelling();
                let command =
                    shlex::try_join(["rimz", "packs", "install", &selected]).map_err(|error| {
                        problem(
                            &self.packs_dir.join("installed.json"),
                            format!("invalid Git source spelling: {error}"),
                        )
                    })?;
                format!(
                    "; retry after `{command}`, which unpins the source, or pin a ref that ships it"
                )
            } else {
                String::new()
            };
            return Err(PackErr::Git {
                source_url: spelling.spelling(),
                message: format!("no pack {name}{at}; available packs: {available}{fix}"),
            });
        }
        if let Some(owner) = self.after.sources.iter().find(|owner| {
            owner.source != source.source && owner.packs.iter().any(|pack| pack.name == *name)
        }) {
            return Err(PackErr::NameTaken {
                name: name.clone(),
                source_path: owner.source.clone(),
            });
        }
        if !source.packs.iter().any(|pack| pack.name == *name) {
            self.after.sources[index].packs.push(InstalledPack {
                name: name.clone(),
                excluded: Exclusions::default(),
            });
            if let Some(parent) = required_by {
                self.required_by.insert(name.clone(), parent.clone());
            }
        }
        Ok(())
    }

    fn requirements(&mut self, index: usize, client: &git::Client) -> Result<()> {
        // Staging only appends sources; queued indices and their held trees remain valid.
        let mut queue: VecDeque<_> = self.after.sources[index]
            .packs
            .iter()
            .map(|pack| (index, pack.name.clone()))
            .collect();
        let mut visited = BTreeSet::new();
        while let Some((index, name)) = queue.pop_front() {
            let parent = self.after.sources[index].clone();
            // A source a requirement moved is visited again at its new commit.
            if !visited.insert((index, parent.commit.clone(), name.clone())) {
                continue;
            }
            let Some(pack) = self.staged[&parent.source]
                .manifest
                .packs
                .iter()
                .find(|pack| pack.name == name)
            else {
                continue;
            };
            let required: Vec<_> = pack
                .requires
                .iter()
                .filter_map(|value| source::requirement(value, &parent.source))
                .collect();
            for dependency in required {
                // The requirement grammar always includes a pack selector.
                let required = dependency.pack.clone().expect("requirement names a pack");
                if self
                    .after
                    .sources
                    .iter()
                    .flat_map(|source| &source.packs)
                    .any(|pack| pack.name == required)
                {
                    continue;
                }
                let index = self.dependency(dependency, &parent, &required, client)?;
                self.select(index, &required, Some(&name))?;
                queue.extend(
                    self.after.sources[index]
                        .packs
                        .iter()
                        .map(|pack| (index, pack.name.clone())),
                );
            }
        }
        Ok(())
    }

    fn dependency(
        &mut self,
        mut source: GitSource,
        parent: &InstalledSource,
        required: &PackName,
        client: &git::Client,
    ) -> Result<usize> {
        let existing = self
            .after
            .sources
            .iter()
            .position(|installed| installed.source == source.source);
        if let Some(index) = existing {
            if !self.staged.contains_key(&source.source) {
                self.hold_installed(index)?;
            }
            let staged = &self.staged[&source.source];
            if !staged.held
                || self.after.sources[index].pinned
                || staged
                    .manifest
                    .packs
                    .iter()
                    .any(|pack| pack.name == *required)
            {
                return Ok(index);
            }
            self.staged.remove(&source.source);
            source.url = self.after.sources[index]
                .url
                .clone()
                .unwrap_or_else(|| source.source.clone());
        }
        let origin = GitSource {
            source: parent.source.clone(),
            url: parent.url.clone().unwrap_or_else(|| parent.source.clone()),
            pack: None,
            reference: None,
        };
        if existing.is_none() && origin.official().is_some() && source.official().is_some() {
            source.reference = parent
                .reference
                .clone()
                .filter(|reference| !is_commit(reference));
        }
        let pinned = parent.pinned && source.reference.is_some();
        let index = self.fetch(&source, client)?;
        self.after.sources[index].pinned = pinned;
        Ok(index)
    }

    pub fn current(&self) -> Result<ResolvedPacks> {
        self.view(&self.before, false)
    }

    pub fn candidate(&self) -> Result<ResolvedPacks> {
        self.view(&self.after, true)
    }

    fn view(&self, record: &Record, candidate: bool) -> Result<ResolvedPacks> {
        let mut roots = BTreeMap::new();
        for source in record.sources.iter().filter(|source| !source.linked) {
            let root = if candidate && let Some(staged) = self.staged.get(&source.source) {
                staged.checkout.clone()
            } else {
                layout::checkout(&self.packs_dir, source)?
            };
            roots.insert(source.entry.clone(), root);
        }
        Ok(resolve_record(&self.packs_dir, record.clone(), &roots))
    }

    pub fn changes(&self) -> Vec<PackChange<'_>> {
        let mut changes = Vec::new();
        for source in &self.after.sources {
            for pack in &source.packs {
                let previous = self
                    .before
                    .sources
                    .iter()
                    .find(|old| old.packs.iter().any(|old| old.name == pack.name));
                if previous.is_some_and(|old| {
                    old.source == source.source
                        && old.url == source.url
                        && old.linked == source.linked
                        && old.reference == source.reference
                        && old.commit == source.commit
                        && old.pinned == source.pinned
                }) {
                    continue;
                }
                changes.push(PackChange {
                    name: &pack.name,
                    source,
                    previous,
                    required_by: self.required_by.get(&pack.name),
                });
            }
        }
        changes
    }

    pub fn up_to_date(&self) -> bool {
        self.before == self.after
    }

    /// Publish only if the record still matches the snapshot reviewed by the caller.
    /// Keep the returned guard through the command to retain every retired checkout.
    pub fn commit(self) -> Result<RetiredCheckouts> {
        let mut retired = RetiredCheckouts { _holds: Vec::new() };
        for source in self.before.sources.iter().filter(|source| !source.linked) {
            let Some(staged) = self.staged.get(&source.source) else {
                continue;
            };
            let checkout = layout::checkout(&self.packs_dir, source)?;
            if checkout != staged.checkout {
                retired
                    ._holds
                    .push(WorkspaceLock::acquire(&checkout.with_extension("lock"))?);
            }
        }
        let _guard = WorkspaceLock::acquire(&self.packs_dir.join("installed.lock"))?;
        if read_record(&self.packs_dir)? != self.before {
            return Err(PackErr::RecordChanged);
        }
        layout::maintain(&self.packs_dir, &self.before)?;
        atomic::sync_dir(&self.packs_dir.join(".checkouts"))?;
        atomic::sync_dir(&self.packs_dir)?;
        for source in &self.after.sources {
            if !self.staged.contains_key(&source.source) {
                continue;
            }
            let entry = self.packs_dir.join(&source.entry);
            let target = layout::checkout_target(&self.packs_dir, source)?;
            if !fs::read_link(&entry).is_ok_and(|current| current == target) {
                atomic::symlink_atomically(&target, &entry)?;
            }
        }
        if self.before != self.after {
            write_record(&self.packs_dir, &self.after)?;
        }
        Ok(retired)
    }
}
