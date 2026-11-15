//! Read-only view of the machine's loop task record, `~/.rimz/loops/tasks.json`.
//!
//! One record per [`TaskId`] holds what RimZ keeps about a task outside its definition: the
//! binding (scope and name) that ties it to a catalog row, arming, the strike count with the fire
//! ids already counted, the scheduled-launch proof, the quiet summary, and per firing checkout
//! the last fire mark. The file is strict and versioned: an unreadable or newer file is an error
//! that stops fires, never an empty record. The locked writer is `task_record_store`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::arming::Arming;
use super::catalog::TaskSource;
use super::run_log::{LoopRunResult, QuietFires};
use crate::disk::paths::loops_dir;
use crate::ids::{FireId, TaskId, WorkflowRunId, WorkspaceId};

pub(super) const VERSION: u32 = 1;
const FILE: &str = "tasks.json";

pub(super) fn path() -> PathBuf {
    loops_dir().join(FILE)
}

/// Which catalog a task's row lives in: the machine's, or one workspace's (its project config
/// and its room instances).
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Machine,
    Workspace(WorkspaceId),
}

impl Scope {
    fn of(source: TaskSource, root: &Path) -> Self {
        match source {
            TaskSource::Config => Self::Machine,
            TaskSource::Instance | TaskSource::Project { .. } => {
                Self::Workspace(WorkspaceId::from_project_root(root))
            }
        }
    }

    /// The scopes a catalog loaded for `project_root` holds every row of.
    pub(super) fn known(project_root: Option<&Path>) -> BTreeSet<Self> {
        let mut scopes = BTreeSet::from([Self::Machine]);
        if let Some(root) = project_root {
            scopes.insert(Self::Workspace(WorkspaceId::from_project_root(root)));
        }
        scopes
    }
}

/// The catalog row a task record belongs to.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Binding {
    pub scope: Scope,
    pub name: String,
}

impl Binding {
    pub(super) fn for_task(name: &str, source: TaskSource, root: &Path) -> Self {
        Self {
            scope: Scope::of(source, root),
            name: name.to_owned(),
        }
    }

    /// `machine::<name>` or `<workspace id>::<name>`: the key the arming, strike and fire views
    /// are indexed by.
    pub fn key(&self) -> String {
        match &self.scope {
            Scope::Machine => format!("machine::{}", self.name),
            Scope::Workspace(workspace) => format!("{workspace}::{}", self.name),
        }
    }
}

/// The last fire of a task in one checkout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FireMark {
    pub fire: FireId,
    pub at: Timestamp,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub booked_by: Vec<crate::agents::LaunchedBy>,
    /// Whether the fire counts for `once-per-checkout`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub once: bool,
    /// The workflow run the fire published, once it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<WorkflowRunId>,
    pub scope: crate::store::workflow::ScopeKind,
    /// The fire's recorded outcome; `None` while it is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<LoopRunResult>,
}

impl FireMark {
    /// Whether the fire still holds its checkout for `once-per-checkout`: it counts, and its
    /// outcome is open or did not fail.
    pub fn holds_checkout(&self) -> bool {
        self.once
            && self.result.is_none_or(|result| {
                matches!(
                    result,
                    LoopRunResult::Completed | LoopRunResult::Launched | LoopRunResult::Delivered
                )
            })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskRecord {
    pub binding: Binding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arming: Option<Arming>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub strikes: u32,
    /// The fire ids whose outcome has moved the strike count, newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counted: Vec<FireId>,
    /// The instant of the newest scheduled launch by firing checkout; the empty key stands for a
    /// fire whose row names no checkout.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scheduled: BTreeMap<String, Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet: Option<QuietFires>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub marks: BTreeMap<PathBuf, FireMark>,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

impl TaskRecord {
    pub(super) fn new(binding: Binding) -> Self {
        Self {
            binding,
            retired_at: None,
            arming: None,
            strikes: 0,
            counted: Vec::new(),
            scheduled: BTreeMap::new(),
            quiet: None,
            marks: BTreeMap::new(),
        }
    }

    pub const fn is_live(&self) -> bool {
        self.retired_at.is_none()
    }

    /// The mark that holds `checkout` for `once-per-checkout`: a project-scoped one from any
    /// checkout, else the checkout's own.
    pub fn launched(&self, checkout: &Path) -> Option<&FireMark> {
        let held = |mark: &&FireMark| mark.holds_checkout();
        self.marks
            .values()
            .filter(held)
            .find(|mark| mark.scope == crate::store::workflow::ScopeKind::Project)
            .or_else(|| self.marks.get(checkout).filter(held))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TaskRecords {
    version: u32,
    #[serde(default)]
    pub(super) tasks: BTreeMap<TaskId, TaskRecord>,
}

impl Default for TaskRecords {
    fn default() -> Self {
        Self::empty()
    }
}

impl TaskRecords {
    pub(super) fn empty() -> Self {
        Self {
            version: VERSION,
            tasks: BTreeMap::new(),
        }
    }

    pub fn get(&self, id: &TaskId) -> Option<&TaskRecord> {
        self.tasks.get(id)
    }

    /// The live record bound to `key` (a [`Binding::key`]).
    pub fn live(&self, key: &str) -> Option<(&TaskId, &TaskRecord)> {
        self.tasks
            .iter()
            .find(|(_, record)| record.is_live() && record.binding.key() == key)
    }

    /// The live record bound to `key`, else the newest retired one: a consumed one-off's.
    pub fn named(&self, key: &str) -> Option<(&TaskId, &TaskRecord)> {
        self.live(key).or_else(|| {
            self.tasks
                .iter()
                .filter(|(_, record)| record.binding.key() == key)
                .max_by_key(|(_, record)| record.retired_at)
        })
    }

    /// The record a task named `name` of the room at `root` is bound to, by [`Self::named`]: the
    /// workspace's binding before the machine's.
    pub fn named_in(&self, root: &Path, name: &str) -> Option<(&TaskId, &TaskRecord)> {
        Scope::known(Some(root))
            .into_iter()
            .rev()
            .find_map(|scope| {
                self.named(
                    &Binding {
                        scope,
                        name: name.to_owned(),
                    }
                    .key(),
                )
            })
    }

    /// The mark that holds `checkout` for a `once-per-checkout` task bound to `key`.
    pub fn launched(
        &self,
        entry: &crate::config::TaskEntry,
        key: &str,
        checkout: &Path,
    ) -> Option<&FireMark> {
        if !entry.dedupes_checkout() {
            return None;
        }
        self.live(key)?.1.launched(checkout)
    }

    /// The ids of the live records bound to a task named `name`, in any scope.
    pub fn ids_named(&self, name: &str) -> Vec<TaskId> {
        self.tasks
            .iter()
            .filter(|(_, record)| record.is_live() && record.binding.name == name)
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// The newest fire mark of each retired record bound in a scope the room at `root` holds,
    /// by name and with no live record of that binding: the fire a consumed one-off's row was
    /// consumed at.
    pub fn consumed(&self, root: &Path) -> BTreeMap<&str, &FireMark> {
        let scopes = Scope::known(Some(root));
        let mut consumed = BTreeMap::<&str, &FireMark>::new();
        for record in self.tasks.values() {
            if record.is_live()
                || !scopes.contains(&record.binding.scope)
                || self.live(&record.binding.key()).is_some()
            {
                continue;
            }
            let Some(mark) = record.marks.values().max_by_key(|mark| mark.at) else {
                continue;
            };
            let name = record.binding.name.as_str();
            if consumed.get(name).is_none_or(|newest| newest.at < mark.at) {
                consumed.insert(name, mark);
            }
        }
        consumed
    }

    /// The live records by binding key.
    pub fn by_key(&self) -> BTreeMap<String, (&TaskId, &TaskRecord)> {
        self.tasks
            .iter()
            .filter(|(_, record)| record.is_live())
            .map(|(id, record)| (record.binding.key(), (id, record)))
            .collect()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TaskRecordErr {
    #[error(
        "cannot read the loop task record {}: {source}; loop tasks do not fire until it reads, so \
         fix its permissions or move it aside (every task then starts armed with no strikes)",
        path.display()
    )]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "the loop task record {} does not decode: {source}; loop tasks do not fire until it \
         reads, so move it aside (every task then starts armed with no strikes)",
        path.display()
    )]
    Decode {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error(
        "the loop task record {} is version {version}, newer than this RimZ reads ({VERSION}); \
         loop tasks do not fire until RimZ is upgraded",
        path.display()
    )]
    Newer { path: PathBuf, version: u32 },
}

impl TaskRecordErr {
    /// What the user runs or does to make the record read again.
    pub fn fix(&self) -> String {
        match self {
            Self::Read { path, .. } => format!(
                "fix the permissions of {}, or move it aside (every task then starts armed with no \
                 strikes)",
                path.display()
            ),
            Self::Decode { path, .. } => format!(
                "move {} aside (every task then starts armed with no strikes)",
                path.display()
            ),
            Self::Newer { .. } => "upgrade RimZ to the version that wrote it".to_owned(),
        }
    }
}

/// The machine's task record; a missing file is an empty one.
pub fn load() -> Result<TaskRecords, TaskRecordErr> {
    load_from(&path())
}

pub(super) fn load_from(path: &Path) -> Result<TaskRecords, TaskRecordErr> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(TaskRecords::empty());
        }
        Err(source) => {
            return Err(TaskRecordErr::Read {
                path: path.to_owned(),
                source,
            });
        }
    };
    #[derive(Deserialize)]
    struct Versioned {
        version: u32,
    }
    let decode = |source| TaskRecordErr::Decode {
        path: path.to_owned(),
        source,
    };
    let Versioned { version } = serde_json::from_slice(&bytes).map_err(decode)?;
    if version > VERSION {
        return Err(TaskRecordErr::Newer {
            path: path.to_owned(),
            version,
        });
    }
    serde_json::from_slice(&bytes).map_err(decode)
}

#[cfg(test)]
impl TaskRecords {
    /// A test record holding one task bound to `binding`, marked for each checkout given.
    pub(super) fn marked(
        binding: Binding,
        marks: impl IntoIterator<Item = (PathBuf, crate::store::workflow::ScopeKind)>,
    ) -> (Self, TaskId) {
        let id = TaskId::new();
        let mut record = TaskRecord::new(binding);
        record.marks = marks
            .into_iter()
            .map(|(checkout, scope)| (checkout, FireMark::open(&id, scope)))
            .collect();
        let mut records = Self::empty();
        records.tasks.insert(id.clone(), record);
        (records, id)
    }
}

#[cfg(test)]
impl FireMark {
    /// An open mark of a fire that counts for `once-per-checkout`.
    pub(super) fn open(task: &TaskId, scope: crate::store::workflow::ScopeKind) -> Self {
        Self {
            fire: FireId::new(task),
            at: Timestamp::now(),
            booked_by: Vec::new(),
            once: true,
            run: None,
            scope,
            result: None,
        }
    }
}

#[cfg(test)]
#[path = "task_record/tests.rs"]
mod tests;
