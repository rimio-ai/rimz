//! Run-note manifests, dated allocation, and the shared dated/legacy archive reader.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use crate::disk::{atomic, paths::Class};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Manifest {
    pub version: u32,
    pub created_at: Timestamp,
    pub checkout: PathBuf,
    #[serde(flatten)]
    pub kind: Kind,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub(crate) enum Kind {
    Team {
        team: String,
        #[serde(flatten)]
        worktree: Option<Worktree>,
    },
    Workflow {
        workflow: String,
        run: crate::ids::WorkflowRunId,
    },
    Loop {
        task: String,
    },
}

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct Worktree {
    pub name: String,
    pub branch: String,
    pub base_branch: Option<String>,
    pub from_pr: Option<u64>,
    pub repo_root: PathBuf,
}

#[derive(Debug, Default)]
pub(crate) struct RunFolders {
    pub runs: Vec<PathBuf>,
    pub dates: Vec<PathBuf>,
}

impl Manifest {
    pub(crate) fn read(run: &Path) -> io::Result<Self> {
        let manifest: Self = read_manifest(run)?;
        if !matches!(manifest.version, 1 | 2) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unknown notes manifest version",
            ));
        }
        Ok(manifest)
    }
}

impl Kind {
    pub(crate) fn read(run: &Path) -> io::Result<Self> {
        read_manifest(run)
    }
}

fn read_manifest<T: serde::de::DeserializeOwned>(run: &Path) -> io::Result<T> {
    let bytes = fs::read(run.join("run.json"))?;
    let mut value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let fields = value.as_object_mut().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "notes manifest must be an object",
        )
    })?;
    if !fields.contains_key("kind") && fields.get("version").is_none_or(|version| version == 1) {
        fields.insert("kind".into(), "team".into());
    }
    serde_json::from_value(value).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

pub(crate) fn allocate(
    paths: &crate::StatePaths,
    kind: Kind,
    checkout: &Path,
    created_at: Timestamp,
) -> io::Result<PathBuf> {
    let lane = run_component(&checkout.file_name().unwrap_or_default().to_string_lossy());
    let (class, label) = match &kind {
        Kind::Team { team, .. } => (Class::TeamNotes, format!("{}-{lane}", run_component(team))),
        Kind::Workflow { workflow, .. } => (
            Class::AutomationNotes,
            format!("workflow-{}-{lane}", run_component(workflow)),
        ),
        Kind::Loop { task } => (
            Class::AutomationNotes,
            format!("loop-{}", run_component(task)),
        ),
    };
    let date = PathBuf::from(created_at.strftime("%Y%m%d").to_string());
    let base = format!("{}-{label}", created_at.strftime("%H%M%SZ"));
    let (relative, run) = claim_run(paths, class, &date, &base, |run| fs::create_dir(run))?;
    atomic::write_temp_then_rename(
        &run.join("run.json"),
        &Manifest {
            version: 2,
            created_at,
            checkout: checkout.to_owned(),
            kind,
        },
    )
    .map_err(io::Error::other)?;
    atomic::sync_dir(&paths.notes_run(class, &date)).map_err(io::Error::other)?;
    Ok(relative)
}

pub(crate) fn migrate_legacy(paths: &crate::StatePaths, run: &Path) -> io::Result<Option<PathBuf>> {
    let Some((stamp, rest)) = run
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.split_once('-'))
        .filter(|(_, rest)| !rest.is_empty())
    else {
        return Ok(None);
    };
    let Ok(created_at) = jiff::civil::DateTime::strptime("%Y%m%dT%H%M%SZ", stamp) else {
        return Ok(None);
    };
    let date = PathBuf::from(created_at.strftime("%Y%m%d").to_string());
    let base = format!("{}-{rest}", created_at.strftime("%H%M%SZ"));
    atomic::sync_tree(run).map_err(io::Error::other)?;
    let (_, moved) = claim_run(paths, Class::TeamNotes, &date, &base, |destination| {
        atomic::publish_staged_no_replace(run, destination).map_err(|error| match error {
            atomic::AtomicErr::Exists { .. } => io::Error::from(io::ErrorKind::AlreadyExists),
            error => io::Error::other(error),
        })
    })?;
    atomic::sync_dir(&paths.team_notes_dir).map_err(io::Error::other)?;
    Ok(Some(moved))
}

fn claim_run(
    paths: &crate::StatePaths,
    class: Class,
    date: &Path,
    base: &str,
    mut claim: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<(PathBuf, PathBuf)> {
    atomic::create_dir_all_mode(&paths.notes_run(class, date), 0o700).map_err(io::Error::other)?;
    let mut suffix = 1;
    let mut recreated_date = false;
    loop {
        let relative = date.join(if suffix == 1 {
            base.to_owned()
        } else {
            format!("{base}-{suffix}")
        });
        let run = paths.notes_run(class, &relative);
        match claim(&run) {
            Ok(()) => return Ok((relative, run)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => suffix += 1,
            Err(error) if error.kind() == io::ErrorKind::NotFound && !recreated_date => {
                atomic::create_dir_all_mode(&paths.notes_run(class, date), 0o700)
                    .map_err(io::Error::other)?;
                recreated_date = true;
            }
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn enumerate(root: &Path) -> io::Result<RunFolders> {
    let mut folders = RunFolders::default();
    for path in directories(root)? {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.len() == 8 && jiff::civil::Date::strptime("%Y%m%d", name.as_bytes()).is_ok() {
            folders.runs.extend(directories(&path)?);
            folders.dates.push(path);
        } else {
            folders.runs.push(path);
        }
    }
    folders.runs.sort();
    Ok(folders)
}

fn directories(root: &Path) -> io::Result<Vec<PathBuf>> {
    match fs::symlink_metadata(root) {
        Ok(metadata) if !metadata.is_dir() => return Ok(Vec::new()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    }
    let mut directories = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => directories.push(entry.path()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    directories.sort();
    Ok(directories)
}

fn run_component(value: &str) -> String {
    let value: String = value
        .chars()
        .take(80)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    let value = value.trim_matches('-');
    if value.is_empty() {
        "run".to_owned()
    } else {
        value.to_owned()
    }
}

#[cfg(test)]
mod tests;
